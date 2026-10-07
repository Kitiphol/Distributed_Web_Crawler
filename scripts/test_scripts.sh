#!/usr/bin/env bash
# Replays the crash-recovery trace from docs/DESIGN.md section 12 against Redis
# and checks the final state. Uses Redis database 15 so real data in database 0 is untouched.
#
# With redis-cli installed locally:     bash scripts/test_scripts.sh
# Without it (scripts are mounted into the Redis container by docker-compose.yml):
#                                       docker compose exec redis bash /scripts/test_scripts.sh
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
R="${REDIS_CLI:-redis-cli} -n 15"
$R FLUSHDB >/dev/null

B=http://localhost:8000/; J=0001; NOW=1791300000; K="job:$J"
sub()  { $R --eval "$DIR/submit.lua" jobs:by_url jobs:active , "$1" "$2" $NOW | tr '\n' ' '; }
take() { $R --eval "$DIR/take.lua" "proc:$1" $K:queue , $J | tr '\n' ' '; }
fin()  { p=$1; shift; $R --eval "$DIR/finish.lua" "proc:$p" $K:seen $K:queue $K:pending $K:stats \
           $K:ext $K:finished $K:meta jobs:active , $J "$@"; }
req()  { $R --eval "$DIR/requeue.lua" "proc:$1"; }
fails=0
check() { if [ "$2" = "$3" ]; then echo "ok    $1"; else echo "FAIL  $1: expected '$3', got '$2'"; fails=1; fi; }

check "submit new"          "$(sub $B $J)"    "new 0001 "
check "submit existing"     "$(sub $B 0002)"  "existing 0001 "
check "A0 takes start URL"  "$(take A:0)"     "0001 $B "
check "A0 finishes /"       "$(fin A:0 $B file html 29 $NOW ${B}a.html ${B}docs/ ${B}logo.svg ${B}missing.html)" "4"
check "B0 takes a.html"     "$(take B:0)"     "0001 ${B}a.html "
check "A1 takes docs/"      "$(take A:1)"     "0001 ${B}docs/ "
check "A2 takes logo.svg"   "$(take A:2)"     "0001 ${B}logo.svg "
check "A3 takes missing"    "$(take A:3)"     "0001 ${B}missing.html "
check "A2 finishes svg"     "$(fin A:2 ${B}logo.svg file svg 0 $NOW)" "3"
check "A3 finishes 404"     "$(fin A:3 ${B}missing.html broken '' 0 $NOW)" "2"
check "A1 finishes docs/"   "$(fin A:1 ${B}docs/ file html 7 $NOW ${B}docs/intro.html)" "2"
# Node B is killed while holding a.html; its heartbeat expires; a reaper requeues it.
check "reaper requeues"     "$(req B:0)"      "0001 ${B}a.html"
check "proc:B:0 now empty"  "$(req B:0)"      ""
check "A0 takes intro"      "$(take A:0)"     "0001 ${B}docs/intro.html "
check "A2 takes a.html"     "$(take A:2)"     "0001 ${B}a.html "
check "A0 finishes intro"   "$(fin A:0 ${B}docs/intro.html file html 12 $NOW ${B}index.html)" "2"
check "A3 takes index"      "$(take A:3)"     "0001 ${B}index.html "
check "A2 finishes a.html"  "$(fin A:2 ${B}a.html file html 11 $NOW ${B}index.html ${B}docs/intro.html)" "1"
check "zombie B0 rejected"  "$(fin B:0 ${B}a.html broken '' 0 $NOW)" "-2"
check "last finish -> 0"    "$(fin A:3 ${B}index.html file html 29 $NOW ${B}a.html ${B}docs/ ${B}logo.svg ${B}missing.html)" "0"
check "status done"         "$($R HGET $K:meta status)"               "done"
check "crawled"             "$($R HGET $K:stats crawled)"             "7"
check "num_files"           "$($R HGET $K:stats num_files)"           "6"
check "words"               "$($R HGET $K:stats total_word_count)"    "88"
check "broken"              "$($R HGET $K:stats broken)"              "1"
check "ext html"            "$($R HGET $K:ext html)"                  "5"
check "ext svg"             "$($R HGET $K:ext svg)"                   "1"
check "pending"             "$($R GET $K:pending)"                    "0"
check "no active jobs"      "$($R SCARD jobs:active)"                 "0"
check "no processing lists" "$($R EVAL "return #redis.call('KEYS', 'proc:*')" 0)" "0"
$R RPUSH proc:D:0 "$J ${B}a.html" >/dev/null
check "duplicate report -1"  "$(fin D:0 ${B}a.html file html 11 $NOW)" "-1"
check "stats unchanged"      "$($R HGET $K:stats num_files)" "6"
$R RPUSH proc:C:0 "$J ${B}x.html" >/dev/null
check "take resumes held"   "$(take C:0)"     "0001 ${B}x.html "
$R FLUSHDB >/dev/null
[ $fails -eq 0 ] && echo "all script tests passed" || { echo "some script tests FAILED"; exit 1; }
