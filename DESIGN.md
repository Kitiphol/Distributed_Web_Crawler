# crawl: design and alternatives

Distributed web crawler for **Mini 1** (spec: https://goms.cs.muzoo.io/projects/mini1.html).

**What this file is for.** It explains *how* the crawler works and *why* it was built this
way, including the alternatives that were considered and rejected. It is not a line-by-line
specification: the code is the source of truth for exact behavior (Lua scripts in `scripts/`,
rules for URLs, words and extensions in `src/domain/`, each with tests).

Contents:
1. What has to be true
2. Architecture
3. State in Redis
4. The life of a URL
5. Design decisions and alternatives
6. Why it is correct
7. Corner-case decisions
8. Limitations and possible extensions

---

## 1. What has to be true

The requirements that shape the design:

| Requirement | Consequence for the design |
| --- | --- |
| Each page is crawled once across the whole cluster | Deduplication must be shared and exact (§5.3) |
| Same `WebStats` for any number of nodes N | Nothing may depend on timing, luck, or N (§5.3, §5.9) |
| The cluster detects completion on its own | An exact "work left" count (§5.4) |
| Several jobs at once, without interfering | Per-job state and fair scheduling (§5.1, §5.7) |
| At most 10 requests in flight per node | 10 worker loops, one URL each |
| The CLI talks only to Redis; nodes can run on any machine | Everything shared lives in Redis (§2) |
| Extra challenge: a node can be killed mid-crawl, every page still counted exactly once | Crash recovery (§5.5, §5.6) |

**Failure model.** A node can be killed at any moment (`kill -9`), or frozen for a while and
then resume. The CLI can be killed at any moment. Websites can be slow or broken. Redis does
not fail (out of scope).

**Guarantees.** No URL is lost. Fetching is at-least-once (a URL may be fetched twice after a
crash); counting is exactly-once. A job is marked done exactly once, and only when all its
work is finished.

## 2. Architecture

```
          crawl submit / status / stats            (short-lived CLI commands)
                         |
                         v
   +----------------------------------------------------+
   |                  Redis (Docker)                    |
   |  jobs:*   job:{id}:*   nodes   node:*:alive  proc:*|
   +----------------------------------------------------+
        ^              ^               ^
        |              |               |       (Redis commands and Lua scripts)
   crawl node A    crawl node B    crawl node C    (N identical processes, any machines)
   10 loops        10 loops        10 loops
   heartbeat       heartbeat       heartbeat
   reaper          reaper          reaper
        |              |               |
        +--------------+---------------+-----> target website (HEAD / GET)
```

- **Three kinds of process**: the CLI, N nodes, one Redis. Nodes never talk to each other or
  to the CLI.
- **Pull model**: one queue per job. A free worker loop takes the next URL itself. There is
  no scheduler process.
- **Inside a node**: a single-threaded Tokio runtime running 10 worker loops, 1 heartbeat
  task and 1 reaper task, sharing one HTTP client and one Redis connection.
- **Atomic updates**: every multi-step update is one Lua script.

## 3. State in Redis

| Key | Type | What it answers |
| --- | --- | --- |
| `jobs:next_id` | int | Next job number |
| `jobs:by_url` | hash url -> id | Was this URL already submitted, and as which job? |
| `jobs:active` | set of ids | Which jobs should workers take from? |
| `job:{id}:meta` | hash | Base path, status (`running`/`done`), timestamps |
| `job:{id}:queue` | list | Claimed URLs waiting to be fetched (the frontier) |
| `job:{id}:seen` | set | Every URL ever claimed in this job |
| `job:{id}:finished` | set | Every URL already reported |
| `job:{id}:pending` | int | Claimed but not finished = queued + in flight |
| `job:{id}:stats`, `job:{id}:ext` | hashes | Counters for `WebStats` |
| `nodes` | hash node id -> loop count | Which nodes must reapers watch? |
| `node:{nid}:alive` | string, expires in 10 s | Is this node alive? |
| `proc:{nid}:{loop}` | list, 0 or 1 items | What is this worker loop holding right now? |

Invariants that every script preserves:

1. `pending == SCARD seen - SCARD finished`.
2. A URL that is claimed but not finished is in exactly one place: its job queue, or one
   processing list.
3. A job is in `jobs:active` exactly while `pending > 0`.

## 4. The life of a URL

1. **Submit.** `crawl submit <url>` creates a job: the URL goes into `seen` and the queue,
   and `pending` becomes 1.
2. **Take.** A free worker loop moves one URL from a job queue into its own processing list.
   The URL is never only in a worker's memory.
3. **Fetch.** The loop sends a HEAD request, and a GET only if the page is HTML. It extracts
   links and counts words.
4. **Finish.** The loop reports the page: the URL leaves the processing list and enters
   `finished`; each new link is claimed (`SADD seen` returns 1) and queued; stats are
   updated; `pending` goes up by the number of new links and down by 1.
5. **Done.** When `pending` reaches 0, the same script marks the job done.

Each step that changes shared state is one script:

| Script | What it does as one step | What would go wrong otherwise |
| --- | --- | --- |
| `submit.lua` | Creates the job, or returns the existing one for that URL | A CLI killed mid-way would leave a URL pointing at a job that never runs |
| `take.lua` | Moves one URL from the first non-empty queue into the loop's processing list | A plain `LPOP` leaves the URL only in the worker's memory, lost on a crash |
| `finish.lua` | Checks ownership, marks finished, claims and queues children, updates stats, decrements `pending`, marks the job done at 0 | A node killed after claiming a child but before queueing it would leave that page claimed forever and never crawled |
| `requeue.lua` | Moves a dead loop's URL back to its job queue | Two reapers could both requeue the same URL |

The order inside `finish.lua` matters:

- **Ownership first.** Only the loop that still holds the URL may report it. Found by
  testing: a node frozen for 16 s woke up with timed-out requests and reported good pages as
  broken. Its report is now rejected, because a reaper had already taken the URL away.
- **Children before the decrement.** New links are added to `pending` before the page itself
  is subtracted, so `pending` cannot touch 0 while work remains.
- **Stats before the decrement.** When a job becomes done, its stats are already complete.

## 5. Design decisions and alternatives

### 5.1 Coordination: per-job queues, pulled by workers

**Chosen:** one queue per job in Redis. Workers pull when they are free.

| Alternative | Why it was not chosen |
| --- | --- |
| One global queue for all jobs | First-come-first-served makes a small job wait behind a big job's backlog, and there is no exact per-job frontier for `crawl status` |
| A scheduler that pushes URLs to nodes | An extra process and point of failure, and an extra hop for every URL (see §5.6) |
| Partitioning URLs across nodes by hash | Only works for a fixed N; a dead node's share of URLs stalls; shares are uneven (see §5.3) |

### 5.2 Atomicity: Lua scripts

The hard operation is claiming a link: `SADD seen <child>`, and **only if that returned 1**,
add to `pending` and push to the queue. A multi-step update like this can break in two ways:

- **Interleaving**: another worker's command runs between the steps and sees a half-done
  state (e.g. `pending` at 0 while a claimed child is not yet counted).
- **Crash**: the worker dies between the steps, leaving a URL claimed and never crawled.

**Chosen:** a Lua script. Redis runs it as one command, it cannot stop halfway once received,
it can branch on results in the middle, and it costs one round trip.

| Alternative | How it works | Why it was not chosen |
| --- | --- | --- |
| `MULTI` / `EXEC` | Queues commands and runs them as one block | Atomic, and one round trip when pipelined, but no result is visible until `EXEC`, so "only if `SADD` returned 1" cannot be expressed. Used here only for read-only status snapshots. |
| `WATCH` + `MULTI` | Read, decide in Rust, then `EXEC`, which aborts if a watched key changed | Correct, but every loop on every node writes `seen`, so transactions would abort and retry constantly. At least two round trips per attempt, and each loop would need its own connection. |
| Distributed lock | `SET lock NX PX`, run the steps, release | About five round trips, and every worker on a job waits for one lock. It stops interleaving but not crashes: a node killed mid-way leaves the half-done state behind when the lock expires. |
| Idempotent design (no claims) | Push every link unchecked, drop duplicates when taking, compute stats at the end | Needs no atomicity, but the queue holds one copy of a URL per page linking to it, stats are not live, and there is no exact `pending`, so completion needs another mechanism. |

One idea from the idempotent design is kept: reporting a URL twice is harmless, thanks to
the `finished` set.

**Cost of Lua:** a second language, and no rollback if a script fails part-way, so the
scripts are short and tested by `scripts/test_scripts.sh`.

### 5.3 Deduplication: one shared set of full URLs

**Chosen:** `job:{id}:seen`, a Redis set of full URLs. It is exact, shared by every node,
and survives any node crash. Its memory cost is small for one site.

| Alternative | Idea | Why it was not chosen |
| --- | --- | --- |
| Bloom filter | A few bits per URL instead of the URL | Approximate: it sometimes reports a new URL as seen, and that page is never crawled. Stats would be slightly wrong, and wrong differently for different N. Its benefit is memory, which matters when crawling many sites, not one. |
| A local set per worker, with URLs routed by hash | `hash(url)` decides the owner, so the owner's local set is enough | A dead worker's set is in its memory and is lost, and its share of URLs stalls. It needs a queue per node and routing. Consistent hashing with virtual nodes evens out URL counts but not work: pages differ in cost, and an idle worker cannot help with another worker's share. |
| A local cache in front of `seen` | A node skips sending URLs it already knows are seen | Tested and rejected. After Redis was brought back with its data cleared, a new job reused a job ID and the node skipped its URLs, because its cache still listed them as seen. Fixable, but children already travel in one batch per page, so the cache saves no round trips. |
| 128-bit hashes instead of URLs | `seen` stores a fixed-size hash of each URL | Exact in practice and smaller, but not needed at this scale. The sets stop being readable in `redis-cli`, and every node must use the same fixed hash function. A possible extension (§8). |

A local set per worker *without* routing does not work at all: two workers would both see
the same URL as new and both count it.

### 5.4 Termination: a `pending` counter

**Chosen:** `pending` = claimed but not finished. It starts at 1, goes up when links are
claimed and down when a page is reported, all inside `finish.lua`. Zero means done.

| Alternative | Why it was not chosen |
| --- | --- |
| "The queue is empty" | Wrong: the queue can be empty while pages still being fetched are about to add links |
| "The queue is empty and nothing is in flight", as two separate reads | Racy unless both are read and updated atomically, which is what `pending` already is, as a single number |
| No activity for some time | A guess: one slow server would end the job early, and results would depend on timing |
| Breadth-first rounds with a barrier between levels | Termination is simple (a round that finds nothing new), but every level waits for its slowest page |

### 5.5 Crash recovery: processing lists, heartbeats, reapers

**Chosen:**

- A taken URL is recorded in the loop's **processing list** in Redis until it is reported.
- Each node refreshes a **heartbeat** key every 3 s; it expires after 10 s without a refresh.
  Expiry is the only part Redis does by itself.
- A **reaper** task on every node checks every 5 s for nodes with no heartbeat and moves
  their processing lists back to the job queues. Recovery takes about 10 to 15 s.
- `finish.lua` rejects reports from a loop that no longer holds the URL, so a frozen node
  that wakes up (a "zombie") cannot affect the stats.

| Alternative | Why it was not chosen |
| --- | --- |
| Redis Streams with consumer groups and `XAUTOCLAIM` | Less code: Redis tracks who holds what and one command reassigns stale entries. But staleness is judged by idle time, so a slow page looks like a dead node and is processed twice. Reading several job streams returns several entries at once, which complicates the limit of 10 in flight. More new concepts, and old entries must be trimmed. |
| `BLMOVE` into a processing list | Keeps the URL in Redis, but takes from a single source list, and there is one queue per job. `take.lua` does the same move across several queues. |
| A status record per URL (queued / in flight / done) | Good for debugging and recovery, but more writes and scans. The aggregate keys are enough. |

### 5.6 Who watches for dead nodes: every node

- **A. Central monitor/scheduler**: one extra process watches heartbeats and requeues.
- **B. Per-node (chosen)**: every node runs its own heartbeat and reaper.
- **C. Distributed monitor**: several monitor replicas elect a leader.

| | A. Central | B. Per-node (chosen) | C. Distributed |
| --- | --- | --- | --- |
| Single point of failure | Yes | No: any surviving node recovers | No |
| Extra processes | One | None | Several, plus leader election |
| Global view for scheduling | Yes | No | Yes |
| Monitoring cost | One scan per interval | One scan per node per interval | One scan per interval |
| Complexity | Low | Low to medium | High |

**Why B.** A and C buy a global view of the cluster, which allows smarter scheduling. This
crawler does not need it. B works because Redis already does the failure detection (the key
expires by itself), and `requeue.lua` is atomic, so several reapers recovering the same node
can neither lose nor duplicate a URL. B is in effect C without the election: instead of
choosing one monitor to act, the action is made safe for all of them to take at once.

**Costs of B.** Every node scans every other node (negligible at this scale). If every node
is dead, nothing is recovered until a node starts again.

In all three, recovery time is set by the heartbeat expiry plus the scan interval, and Redis
remains a single point of failure.

### 5.7 Fairness between jobs: shuffle on every take

**Chosen:** before each take, the loop shuffles the list of active jobs; `take.lua` uses the
first non-empty queue. Each job with waiting URLs gets an equal share on average, with no
coordination between loops or nodes.

| Alternative | Why it was not chosen |
| --- | --- |
| Fixed rotation | When a job's queue is empty, its turns always go to the same neighbor, so shares are uneven |
| One global queue | A small job waits behind a big job's backlog |

Scheduling affects only speed and order, never the results.


### 5.8 Fetching

- **HEAD first, GET only for HTML.** Non-HTML files only need to be counted, not downloaded.
- **Redirects are not followed.** The target is treated as a new link, so it goes through
  the same base-path check and deduplication as any other URL.
- **Temporary failures are retried** (network errors, timeouts, 5xx, 429): up to 3 attempts,
  pausing 250 ms then 500 ms. Permanent errors such as 404 are not. Found by testing: without
  retries, one hiccup turned a good page into a broken one, and totals differed between runs.
- **A broken URL is still reported**, so it is counted and `pending` is decremented. A dead
  link cannot keep a job from finishing.
- Each request has a 10 s timeout, so a hanging server cannot hold a loop forever.

## 6. Why it is correct

- **Claim once.** A URL enters a queue only when `SADD seen` returns 1, which happens once
  per URL per job.
- **Take once.** `take.lua` moves a URL into exactly one processing list.
- **Nothing lost.** A taken URL stays in Redis until `finish.lua` removes it in the same
  step that records it as finished. If its node dies, a reaper puts it back.
- **Count once.** `finish.lua` accepts a report only from the current holder, and counts
  only if `SADD finished` returns 1.
- **Termination.** `pending` = claimed - finished. Children are added before the parent is
  subtracted, so it is never 0 while work remains, and once 0 nothing can raise it.
- **Same answer for any N.** The set of claimed URLs is determined by the site, not by
  timing. Stats are sums over that set, and sums do not depend on order.
- **Isolation.** All per-job state is under `job:{id}:`.

| Crash moment | Effect | Recovery |
| --- | --- | --- |
| Before `take.lua` | Nothing taken | None needed |
| Between `take.lua` and `finish.lua` | URL is in a processing list, still counted in `pending` | Heartbeat expires; a reaper requeues it |
| During a script | Impossible: scripts are atomic | — |
| After `finish.lua` | Fully reported | None needed |
| Reaper killed mid-recovery | Each requeue is atomic per item | Another reaper continues |
| CLI killed during submit | The job is fully created or not at all | Resubmit |
| Node frozen > 10 s, then resumes | Its URL was requeued and may be fetched twice | Its late report is rejected; only the new holder's counts |

Tested by `cargo test` (rules), `scripts/test_scripts.sh` (scripts) and
`scripts/e2e_test.sh` (1 vs 3 nodes, two jobs, `kill -9`, and a frozen-then-resumed node,
all giving the same stats).

## 7. Corner-case decisions

- **Normalization.** Fragments are dropped; query strings are kept; only `http` and `https`.
  No trailing-slash or `index.html` rewriting, so `/` and `/index.html` are different URLs.
- **Base path.** A URL is in scope if it starts with the submitted URL, as the spec says.
  A base without a trailing slash (`http://h/docs`) therefore also matches `http://h/docs-old`.
- **Extension.** Taken from the URL's last path segment, lowercased; none means `html`.
  It is decided by the URL, not by `Content-Type`.
- **HTML detection.** By `Content-Type` (`text/html` or `application/xhtml+xml`).
- **Links.** `href` and `src` from links, images, scripts, stylesheets and media, because
  the spec counts every linked file, not just pages.
- **Words.** Text nodes only (no tags, attributes, comments, scripts or styles), lowercased,
  split on non-alphanumeric characters, counting tokens that start with `a`-`z`.

## 8. Limitations and possible extensions

- Taking is polling-based (scripts cannot block): up to about 500 ms pickup delay when idle.
- A node frozen longer than 10 s causes a duplicate fetch (counted once).
- Scripts do not roll back on a runtime error part-way.
- Redis restarts are survived; Redis losing its data is not.
- robots.txt, politeness delays, JavaScript rendering and job cancellation are out of scope.
- Words split across inline tags (`Hel<b>lo</b>`) count as two words.
- Possible extensions: job priorities or weights; fetching HTML pages first; storing
  128-bit URL hashes in `seen` (§5.3); pub/sub for `status -f`.
