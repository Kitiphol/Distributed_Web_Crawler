#!/usr/bin/env bash
# End-to-end tests: correctness with 1 vs 3 nodes, two jobs at once, a node killed
# mid-crawl, and a node frozen and resumed (zombie). Uses Redis database 14, so data
# in database 0 is untouched.
#
# Needs: Redis running (docker compose up -d), python3, and redis-cli. Without a local
# redis-cli, use the one inside the container:
#   REDIS_CLI="docker compose exec -T redis redis-cli" bash scripts/e2e_test.sh
set -u
cd "$(dirname "$0")/.."
RC="${REDIS_CLI:-redis-cli} -n 14"
export REDIS_URL="${REDIS_URL_BASE:-redis://127.0.0.1:6379}/14"

cargo build --release -q || exit 1
B=./target/release/crawl
fails=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; fails=1; }

python3 -m http.server 8000 --directory test-site >/dev/null 2>&1 & HTTP=$!
python3 scripts/make_big_site.py /tmp/crawl-test-big 200 >/dev/null
python3 scripts/slow_server.py /tmp/crawl-test-big 8001 600 >/dev/null 2>&1 & SLOW=$!
python3 scripts/slow_server.py test-site 8002 0 flaky >/dev/null 2>&1 & FLAKY=$!
trap 'kill $HTTP $SLOW $FLAKY 2>/dev/null' EXIT
sleep 1

pids=()
start_nodes() { pids=(); for i in $(seq 1 "$1"); do $B node > "/tmp/crawl-node$i.log" 2>&1 & pids+=($!); done; sleep 0.5; }
stop_nodes()  { for p in "${pids[@]}"; do kill -CONT "$p" 2>/dev/null; kill -INT "$p" 2>/dev/null; done; wait "${pids[@]}" 2>/dev/null; }
fresh()       { $RC FLUSHDB >/dev/null; }
# Wait until a job is done (portable: macOS has no `timeout` command). Gives up after ~3 minutes.
wait_done()   { for _ in $(seq 1 360); do $B status "$1" 2>/dev/null | grep -q " done$" && return 0; sleep 0.5; done
                echo "gave up waiting for job $1 after 3 minutes" >&2; return 1; }
crawl()       { $B submit "$1" >/dev/null; wait_done "${2:-0001}"; $B stats "${2:-0001}"; }

echo "== test site: 1 node vs 3 nodes"
fresh; start_nodes 1; one=$(crawl http://localhost:8000/); stop_nodes
fresh; start_nodes 3; three=$(crawl http://localhost:8000/); stop_nodes
expected=$'files: 6   extensions: 2   words: 88\n  html 5   svg 1'
[ "$one" = "$expected" ] && pass "1 node gives the expected stats" || fail "1 node: got: $one"
[ "$three" = "$expected" ] && [ "$one" = "$three" ] && pass "3 nodes give the same stats" || fail "3 nodes: got: $three"

echo "== two jobs at once, plus a resubmission"
fresh; start_nodes 3
out=$($B submit http://localhost:8000/ http://localhost:8000/extra/ http://localhost:8000/)
wait_done 0001; wait_done 0002
[ "$(echo "$out" | tail -1)" = "job 0001  http://localhost:8000/  (already submitted)" ] && pass "resubmission returns the existing job" || fail "resubmission: $out"
[ "$($B stats 0001)" = "$expected" ] && pass "job 0001 unaffected by job 0002" || fail "job 0001: $($B stats 0001)"
listed=$($B status | grep -c " done ")
[ "$listed" = "2" ] && pass "crawl status lists both jobs as done" || fail "crawl status listed: $($B status)"
extra=$'files: 8   extensions: 4   words: 98\n  html 5   css 1   js 1   png 1'
[ "$($B stats 0002)" = "$extra" ] && pass "extra rules (src/href links, redirect, query, broken, outside)" || fail "job 0002: $($B stats 0002)"
stop_nodes

echo "== flaky server: every URL fails once with 503, so the crawler must retry"
fresh; start_nodes 3; flaky=$(crawl http://localhost:8002/); st=$($B status 0001); stop_nodes
[ "$flaky" = "${expected//8000/8002}" ] && pass "temporary failures are retried" || fail "flaky server: got: $flaky ($st)"

echo "== big site (slow server): clean run"
fresh; start_nodes 3; clean=$(crawl http://localhost:8001/); st=$($B status 0001); stop_nodes
echo "$clean"
big=$'files: 221   extensions: 2   words: 2985\n  html 201   png 20'
[ "$clean" = "$big" ] && pass "big site gives the expected stats" || fail "big site: got: $clean ($st)"

echo "== big site: kill -9 a node while it holds URLs"
fresh; start_nodes 3
N2=$(grep -o 'node [0-9a-f]* started' /tmp/crawl-node2.log | cut -d' ' -f2)
held() { $RC EVAL "local n=0 for _,k in ipairs(redis.call('KEYS','proc:$N2:*')) do n=n+redis.call('LLEN',k) end return n" 0; }
$B submit http://localhost:8001/ >/dev/null
for _ in $(seq 1 100); do [ "$(held)" -ge 2 ] && break; sleep 0.1; done
echo "killing node $N2 while it holds $(held) URL(s)"; kill -9 "${pids[1]}"
wait_done 0001; killed=$($B stats 0001); st=$($B status 0001); stop_nodes
[ "$killed" = "$clean" ] && pass "killed node: same stats as the clean run" || fail "killed node: got: $killed ($st)"
grep -h "forgot dead node" /tmp/crawl-node1.log /tmp/crawl-node3.log | head -1

echo "== big site: freeze a node for 16 s, then resume it (zombie)"
fresh; start_nodes 3
N2=$(grep -o 'node [0-9a-f]* started' /tmp/crawl-node2.log | cut -d' ' -f2)
$B submit http://localhost:8001/ >/dev/null
for _ in $(seq 1 100); do [ "$(held)" -ge 2 ] && break; sleep 0.1; done
echo "freezing node $N2 while it holds $(held) URL(s)"
kill -STOP "${pids[1]}"; sleep 16; kill -CONT "${pids[1]}"
wait_done 0001; zombie=$($B stats 0001); st=$($B status 0001); stop_nodes
[ "$zombie" = "$clean" ] && pass "zombie node: same stats as the clean run" || fail "zombie: got: $zombie ($st)"
echo "zombie's late reports rejected: $(grep -c 'no longer ours' /tmp/crawl-node2.log)"

fresh
[ $fails -eq 0 ] && echo "all end-to-end tests passed" || { echo "some end-to-end tests FAILED"; exit 1; }
