# crawl: a distributed web crawler

`crawl` crawls every page under a base path and reports statistics about what it finds
(`WebStats`: number of files, files per extension, and total words in HTML pages).
The work is shared by any number of crawler nodes that coordinate **only through Redis**.
One binary provides both the nodes and the command-line tool.

Nodes can be killed mid-crawl (or frozen and resumed): the job still finishes, and every
page is counted exactly once.



### Note: Read DESIGN.md to understand the choices made

### 1. Start Redis
```bash
docker compose up -d
```
or, without compose:
```bash
docker run -d --name crawl-redis -p 6379:6379 redis:7 redis-server --appendonly yes
```

### 2. Build and install
```bash
cargo install --path .
```
This builds a release binary and puts `crawl` on your PATH. To build without installing, run
`cargo build --release` and use `./target/release/crawl` wherever the examples say `crawl`.

### 3. Check the connection
```bash
crawl ping
```
```
redis replied PONG
```
Every command connects to `redis://127.0.0.1:6379` by default. To use a Redis somewhere else,
see [Running on several machines](#running-on-several-machines).

### 4. Launch N nodes
Run this once per node, each in its own terminal:
```bash
crawl node
```
Add `-q` to hide the per-page log. Stop a node with Ctrl+C: it finishes the pages it's working
on and leaves the cluster.

### 4.1 Why there is no monitor or scheduler service

Three ways to detect dead nodes and recover their URLs were considered:

- **A. Central monitor/scheduler**: one extra process watches heartbeats, requeues dead
  nodes' URLs, and possibly assigns work to nodes.
- **B. Per-node (chosen)**: every node refreshes its own heartbeat and runs its own reaper;
  nodes pull work themselves.
- **C. Distributed monitor/scheduler**: several monitor replicas elect one leader to do the
  job; another takes over if the leader dies.

| | A. Central | B. Per-node (chosen) | C. Distributed |
| --- | --- | --- | --- |
| Single point of failure | Yes: if it dies, nothing is recovered (and nothing is crawled, if it also assigns work) | No: any surviving node recovers | No |
| Extra processes to run | One | None | Several, plus leader election |
| Recovery logic | One place, no duplicate work | In every node; must be safe when several reapers act at once | One leader; must still be safe during a leader change |
| Global view for scheduling | Yes | No | Yes |
| Monitoring cost | One scan per interval | One scan per node per interval | One scan per interval |
| Per-URL overhead | An extra hop if it assigns every URL | None: nodes pull directly | Same as A |
| Complexity | Low | Low to medium | High |

**Why B.** A and C buy a global view of the cluster, which allows smarter scheduling
(priorities, load balancing, per-site rate limits). This crawler does not need it: fairness
is a random shuffle that each loop can do alone (§13). B works because of two properties the
design already has:

- Redis does the failure detection: a heartbeat key expires by itself (§9.4).
- `requeue.lua` is atomic (§7.4), so several reapers recovering the same dead node can
  neither lose nor duplicate a URL.

B is in effect C without the election: every node is a monitor, and instead of choosing one
to act, the action is made safe for all of them to take at once. A coordinator would add a
point of failure without adding correctness.

**Costs of B.** Every node scans every other node, so monitoring work grows with the square
of the node count (negligible at this scale). Scheduling is limited to what each loop can
decide locally. If every node is dead, nothing is recovered until a node starts again.

**The same in all three.** Recovery time is set by the heartbeat TTL plus the reaper
interval (about 10 to 15 s, §14), not by who does the scanning. Redis is a single point of
failure in every option; its failure is out of scope (§3).

### 5. Use the CLI
Submit one or more URLs. Each becomes a job:
```
$ crawl submit https://example.org/docs/ https://example.com/api/
job 0001  https://example.org/docs/
job 0002  https://example.com/api/
```
Check one job, or follow it until it's done with `-f`:
```
$ crawl status 0001
job 0001  crawled 152   frontier 37   in flight 20   files 150   broken 2   running

$ crawl status -f 0001
...
job 0001  crawled 1543   frontier 0   in flight 0   files 1540   broken 3   done
```
List every job (oldest first) by leaving out the ID. With `-f`, the table refreshes until every
job is done:
```
$ crawl status
job    status    crawled  frontier  in flight   files  broken  url
0001   running       152        37         20     150       2  https://example.org/docs/
0002   done           48         0          0      48       0  https://example.com/api/
```
Print the statistics of a finished job:
```
$ crawl stats 0001
files: 1540   extensions: 6   words: 1283077
  html 1391   png 88   js 41   css 12   pdf 6   jpg 2
```

| Column | Meaning |
| --- | --- |
| crawled | URLs fetched and reported so far |
| frontier | URLs waiting in the queue |
| in flight | URLs a worker loop is fetching right now |
| files | URLs that exist (not broken, not redirects) |
| broken | URLs that don't exist or kept failing |

Submitting a URL that was already submitted prints the existing job's ID.
`crawl stats` on a running job says so; stats stay in Redis until it's cleared.

### Try it locally
Terminal 1, a small test site:
```bash
python3 -m http.server 8000 --directory test-site
```
Terminal 2, a node:
```bash
crawl node
```
Terminal 3:
```bash
crawl submit http://localhost:8000/
crawl status -f 0001
crawl stats 0001
```
```
files: 6   extensions: 2   words: 88
  html 5   svg 1
```

### Running on several machines
Every command accepts `--redis <url>` **before the subcommand**, or the `REDIS_URL`
environment variable:
```bash
crawl --redis redis://192.168.1.50:6379 ping
crawl --redis redis://192.168.1.50:6379 node
```
```bash
export REDIS_URL=redis://192.168.1.50:6379
crawl node
```
One machine runs Redis; every other machine needs only the `crawl` binary and that address.
Nodes never talk to each other, so nothing else changes. Redis's port (6379) must be reachable
from the other machines. On a network that isn't private, give Redis a password
(`--requirepass`) and use `redis://:<password>@<host>:6379`.

### Useful commands while it runs
```bash
pgrep -fl "crawl node"
docker compose exec redis redis-cli HKEYS nodes
docker compose exec redis redis-cli FLUSHALL
```
The first lists the node processes on this machine; the second lists the nodes registered in
Redis; the third deletes every job (nodes can keep running).

## How it works

### The pieces
- **Redis** holds everything shared: jobs, queues, counters, and which node holds which URL.
- **`crawl node`** (any number): each runs 10 worker loops on one thread, plus a heartbeat
  and a reaper. Each loop repeats: take one URL, fetch it, build its report, report it.
- **The CLI** (`submit`, `status`, `stats`) reads or writes Redis and exits. It never talks to
  the nodes, and nodes never talk to each other.

### How the nodes coordinate
- **Pull, don't push.** There is no scheduler. A worker loop takes the next URL when it's free.
- **One waiting list (queue) per job.** To take work, a loop reads the running jobs, shuffles
  them, and moves one URL from the first non-empty queue into its own **processing list** in
  Redis. Shuffling gives every job an equal share, so a small job isn't stuck behind a big one.
- **Each page is crawled once.** A URL is *claimed* by adding it to the job's `seen` set; Redis
  says whether it was newly added, so exactly one loop wins and only the winner queues it.
- **Atomic steps.** Multi-step updates are small Lua scripts (`scripts/`), which Redis runs as a
  single step that can't be interrupted or interleaved. Reporting a page is one script: claim and
  queue its new links, add its statistics, and update the job's pending count, all at once.
- **At most 10 requests in flight per node**: 10 loops, each holding one URL at a time.

### How progress is tracked
| Key | What it holds |
| --- | --- |
| `jobs:by_url`, `jobs:active`, `job:{id}:meta` | which URL became which job; running jobs; base path and status |
| `job:{id}:queue` | URLs waiting to be fetched (the **frontier**) |
| `job:{id}:seen` | every URL ever claimed |
| `job:{id}:finished` | every URL already reported |
| `job:{id}:pending` | claimed but not finished = waiting + in flight |
| `job:{id}:stats`, `job:{id}:ext` | counters: crawled, files, broken, redirects, words; files per extension |
| `proc:{node}:{loop}` | the one URL a worker loop is holding (0 or 1 items) |
| `nodes`, `node:{node}:alive` | registered nodes; heartbeats that expire after 10 s |

`crawl status` reads these in one atomic snapshot: frontier = queue length;
in flight = pending - frontier.

### How the cluster knows a job is done
An empty queue isn't enough: a page in progress may still add links. So each job has a
**pending** counter: URLs claimed but not finished. The report script adds a page's new links to
pending **before** subtracting the page itself, so pending can't reach 0 while any work remains,
and once it's 0 nothing can raise it again. Redis's decrement is atomic, so exactly one report
sees 0, and that report marks the job done. A page's statistics are written in the same step, so
the stats are complete the moment the job is done. The result doesn't depend on N or on timing:
every linked URL under the base path is claimed exactly once, and the stats are sums.

### Surviving a crashed node
- A taken URL lives in the loop's processing list **in Redis**, not just in the node's memory.
- Every node renews a heartbeat key every 3 s; Redis deletes it 10 s after the last renewal.
- Every node runs a **reaper**: every 5 s it finds registered nodes without a heartbeat and moves
  their URLs back to the queues. Pending never changed, so the job waits for them.
- **Several reapers can't collide.** Moving one URL back is a single script (pop it from the dead
  node's list, push it onto the queue), so when two nodes notice the same death, each URL is moved
  by exactly one of them and the other finds the list empty.
- **Ownership**: a report is accepted only from the loop that still holds the URL. If a node
  froze (rather than died) and wakes up after its URLs were recovered, its late reports are
  rejected. (Testing found that a frozen node's requests time out, so without this rule it could
  report good pages as broken.)
- A URL is reported at most once (`finished` set), so a URL fetched twice is still counted once.

### Surviving a Redis restart
- Every Redis command has a timeout (5 s to answer, 2 s to connect), so nothing hangs forever.
- If a node's heartbeat fails, the node opens a fresh connection, shared by all its loops, and
  prints `Redis is back` when it succeeds. Worker loops retry failed commands, so the crawl
  continues once Redis is back.
- Data survives a restart because Redis saves to disk (`--appendonly yes` in docker-compose.yml).
  `docker compose down -v` deletes that data, and with it every job.
- If Redis is down longer than 10 s, heartbeats expire and nodes briefly look dead to each
  other; a reaper may put back some URLs. The ownership check keeps this harmless.
- Nodes keep no job state in memory: a loop reads the job's base path from Redis for every
  URL. (Testing found that an earlier version, which cached it per job ID, crawled only the
  seed page after Redis was wiped, because the ID was reused for a different site.)

### Fewer round trips
- Each node keeps one Redis connection open and reuses it for every command; scripts are sent
  by their hash, not their full text.
- Each worker loop re-reads the list of running jobs at most once per second (new jobs are
  noticed within a second), so a URL costs three Redis round trips: take, read the job's base
  path, and report.
- HTTP connections to the crawled site are kept alive and reused by all 10 loops (up to 10 idle
  connections per site), so only the first request pays for the TCP and TLS handshake.

## Decisions on corner cases
| Case | Decision |
| --- | --- |
| Which links count | `href` of `<a>`, `<area>`, `<link>`; `src` of `<img>`, `<script>`, `<iframe>`, `<source>`, `<embed>`, `<audio>`, `<video>`, since CSS, JS and images are files too |
| Relative links | Resolved against the page's URL (or its `<base href>`, if the page has one) with the `url` crate |
| Fragments | Dropped (`page.html#intro` = `page.html#` = `page.html`); a link to `#section` on the same page is not a new URL |
| Query strings | Kept (`?page=2` is a different URL) |
| `/` vs `/index.html`, trailing slashes | Different URLs; the crawler doesn't guess what a server does. If the server redirects one to the other, only the target counts as a file |
| Letter case | Scheme and host are lowercased (they're case-insensitive); the path keeps its case, since `/Page.html` and `/page.html` can be different files |
| Percent-encoding | Handled by the `url` crate: a raw space and `%20` become the same URL. Other encoded forms (like `%7E` vs `~`) are kept as written |
| Same file linked from many pages | Counted once: each URL is claimed once per job, no matter how many pages link to it (`<img src>` and `<a href>` to the same file count once too) |
| Links not followed | `srcset` image candidates, `url(...)` inside CSS files, and `<meta http-equiv="refresh">` |
| Non-http(s) links (`mailto:`, `javascript:`, `tel:`, `data:`) | Ignored |
| Base path | A URL must start with the normalized base URL (string prefix, as the spec says) |
| HTML or not | Decided by `Content-Type` (`text/html`, `application/xhtml+xml`) |
| Non-HTML files | Checked with HEAD only; never downloaded |
| Servers that reject HEAD (405/501) | Fall back to GET |
| Redirects | Not followed automatically; the redirecting URL isn't a file, and its target is treated as a newly found link (if under the base path) |
| Redirect leaving the base path | Not followed. For example, if `http://site/` redirects everything to `https://site/`, the targets are outside the base, so submit the `https` URL instead |
| Temporary failures | Network errors, timeouts (10 s), 5xx and 429 are retried up to 3 times (waiting 250 ms, then 500 ms) before the URL counts as broken |
| Broken links | Permanent errors like 404 aren't retried; not files; counted as broken |
| Extension | From the URL's last path segment, lowercased; no extension means `html` (decided by URL, not `Content-Type`). So `/index.php` is listed under `php` even though it is parsed as HTML |
| Words | Text nodes only (so no tags, attributes or comments), skipping `<script>`, `<style>`, `<noscript>`; lowercased, split on non-alphanumerics, counting tokens that start with `a`-`z` |
| Resubmitting a URL | Matched after normalization; returns the existing job |

## Testing

### Automated
```bash
cargo test
bash scripts/test_scripts.sh
bash scripts/e2e_test.sh
```
- `cargo test`: unit tests for all the rules above.
- `test_scripts.sh`: the Lua scripts, including a crash scenario.
- `e2e_test.sh`: the whole system: 1 node vs 3 nodes, two jobs at once, `kill -9` a node,
  freeze and resume a node, a server whose URLs fail once.

Without a local `redis-cli`, run the script tests inside the container
(`docker compose exec redis bash /scripts/test_scripts.sh`) and the end-to-end tests with
`REDIS_CLI="docker compose exec -T redis redis-cli" bash scripts/e2e_test.sh`.
Both test scripts use separate Redis databases (14 and 15), so they don't touch your data.

### A bigger, slower site
```bash
python3 scripts/make_big_site.py test-big 200
python3 scripts/slow_server.py test-big 8001 300
crawl submit http://localhost:8001/
```
The last argument of `slow_server.py` is the delay per request in milliseconds. The expected
result is `files: 221   extensions: 2   words: 2985` (`html 201   png 20`). To test retries,
`python3 scripts/slow_server.py test-site 8002 0 flaky` serves a site where every URL fails once.

### Killing a node by hand
Start three nodes, each in its own terminal, writing their logs to files. Start the one you'll
kill last:
```bash
crawl node 2> node-a.log
```
```bash
crawl node 2> node-b.log
```
```bash
crawl node 2> node-c.log
```
In a fourth terminal, check that exactly three are running, then start a crawl:
```bash
pgrep -fl "crawl node"
crawl submit http://localhost:8001/
```
While it's running, kill the newest node without giving it a chance to clean up, and follow
the job:
```bash
pkill -9 -n -f "crawl node"
crawl status -f 0001
```
What to expect:
- The killed node's terminal prints `killed`.
- `in flight` stays above 0 for up to about 15 s (the dead node's URLs), then the job finishes.
- A survivor's log shows the recovery:
  ```bash
  grep -h "dead" node-a.log node-b.log
  ```
  ```
  [1fb07b09 reaper] node 9c90f010 is dead; put back: 0001 http://localhost:8001/page-17.html
  [1fb07b09 reaper] forgot dead node 9c90f010 (10 URL(s) recovered)
  ```
- `crawl stats 0001` prints the same numbers as a run where nothing was killed.

To test a frozen node instead, use `kill -STOP <pid>`, wait 16 s, then `kill -CONT <pid>`: its
log shows the late reports being rejected (`no longer ours`).

## Project layout
```
src/
  main.rs, cli.rs, config.rs
  domain/      pure rules (no I/O): URLs, extensions, words, HTML parsing, reports, WebStats
  store/       the only code that talks to Redis: key names, Lua scripts, operations
  crawler/     fetcher, worker loop, heartbeat, reaper
  commands/    node, submit, status, stats, ping
scripts/       Lua scripts, test scripts, test-site generator, slow test server
test-site/     small sites used by the tests
docs/          DESIGN.md (full design), CONVERSATION_LOG.md (how it was decided)
```
