# Conversation log: how the design was reached

**What this file is for.** `DESIGN.md` says *what* to build. This file records *why*: the
questions asked, the explanations given, the alternatives considered and rejected, and the
misunderstandings that were cleared up along the way. Read it to understand the reasoning
behind a decision before changing it, and to know what the student already understands.
It is a detailed summary of a long conversation between the student and Claude (claude.ai),
in chronological order, not a word-for-word transcript. `DESIGN.md` wins on any conflict.

---

## 0. About the student

- Doing Mini 1 (distributed web crawler) individually, in Rust, and must explain the design in
  the README and in a live demo. Wants to understand everything, not just receive code.
- Started out new to async Rust (Tokio) and to Redis. Learned the concepts during this
  conversation (sections 3-4); still benefits from explanations of anything non-trivial.
- Thinks carefully about alternatives and challenges designs (questioned Lua, proposed bit
  arrays, a global queue, a central scheduler); good decisions came from these challenges.
- Plans to use Claude Code to implement the design.

## 1. The first design: a web-scale crawler (discarded)

**Question:** "Help me design a distributed web crawler: functional and non-functional
requirements, components, what to use and why."

**What Claude produced (before seeing the spec):** a design for crawling the open web at
~1 billion pages/month: functional requirements (seeds, URL normalization and dedup, robots.txt,
politeness, storage, recrawling, content dedup), non-functional ones (scalability, fault tolerance,
politeness, robustness to spider traps), back-of-envelope numbers (~400 pages/s average,
~100 TB/month raw), and components: a Mercator-style URL frontier (front queues for priority,
per-host back queues plus a ready heap for politeness), host partitioning via Kafka keys,
async fetchers, DNS cache, robots.txt cache, parser, Bloom-filter URL dedup, SimHash content
dedup, WARC files in S3, Cassandra metadata, a recrawl scheduler, a JS-rendering pool,
Prometheus/Grafana. Also explained the frontier in depth (two-tier queues, delay =
max(crawl-delay, 10 x last fetch time), back queues ~3x fetcher threads).

**Student's challenges:**
- "Shouldn't the URL dedup filter be in shared infrastructure?" Answer: with host partitioning,
  a URL always reaches the same node, so a local Bloom filter is globally correct; but content
  dedup crosses hosts and does belong in a shared service. (Half right.)
- "I'm doing this in Rust." The design was mapped to Rust crates (tokio, reqwest, rdkafka,
  redis, scylla, lol_html/scraper, url, hickory-resolver, etc.), with Rust-specific advice:
  never parse HTML on Tokio threads for long, actor-per-partition, bounded channels.
- "I still don't get the design": redrawn as three views (system, partitioning, inside a node).

**Then the spec was shared** (https://goms.cs.muzoo.io/projects/mini1.html), and almost all of the
above was dropped:

| Web-scale design | Mini 1 reality |
| --- | --- |
| Billions of pages, store everything | One site under a base path; output is just `WebStats` |
| Kafka + Cassandra + S3 | Only Redis |
| Partition by host | One site = one host: partitioning would put all work on one node |
| Bloom filter (approximate is fine) | Must be exact: approximate breaks "same answer for any N" |
| Runs forever, recrawls | Finite jobs that must detect their own completion |
| Politeness, robots.txt | Out of scope; just a cap of 10 in flight per node |

What carried over: the crawl loop (frontier -> fetch -> parse -> new links), async fetching,
URL normalization, identical nodes coordinating through shared state.

## 2. Push scheduler vs. pull model

**Student's first mental model:** the CLI puts the URL in a frontier queue; a scheduler (in the
CLI) allocates work to workers, each with its own queue; the scheduler watches worker queues and
redistributes work if something goes wrong.

**Corrections:**
- The CLI is short-lived (`submit` returns immediately), so it can't be a scheduler.
- **Pull model chosen:** one shared queue (per job); a worker takes the next URL whenever it is
  free. Taking *is* the assignment; Redis guarantees each item goes to one taker.
- Benefits: automatic load balancing (no worker stuck with a backlog while others idle), simpler
  termination, no single point of failure, identical nodes.
- The link filter condition was also stated backwards by the student: a link is queued only if it
  **is** under the base path **and** has **not** been seen.

**"Tokio does scheduling though":** Tokio schedules *tasks onto threads within one process*;
Redis distributes *URLs across processes*. Two different levels. (Tokio's multi-threaded
scheduler is itself pull-based: idle threads steal work.) A dispatcher *inside* a node is fine;
a central scheduler *across* the cluster is what was rejected.

## 3. Learning Tokio and concurrency

This took many turns; the student repeatedly asked for clarification. Key points established:

- **Terminology settled with the student:** a **worker** (or worker node) = one `crawl node`
  process on one thread; a **task** (in the student's wording) = one URL being crawled;
  in code, each of the 10 slots is a long-lived loop. Ten loops exist from startup; they never
  finish; each pulls one URL, processes it fully, then pulls the next.
- **Misconception corrected:** "when a task hits I/O, the worker pulls another task" -> No.
  The number of in-progress URLs is fixed by the number of loops (10). Hitting I/O makes Tokio
  switch to another *existing* loop; it does not pull new work.
- **Misconception corrected:** "a task waiting on I/O is requeued to Redis" -> No. It stays
  paused in the worker's memory and resumes where it left off. Requeueing would lose progress,
  cause duplicate fetches, and churn.
- **Misconception corrected:** "need a local queue of size 10" -> No local queue. Pull only when a
  slot frees; a local buffer would hide work from idle nodes (worst near the end of a crawl).
- **`.await`:** "this task waits; the thread doesn't." It looks sequential inside one loop;
  concurrency comes from several loops waiting at the same time. `.await` is the point where a
  task hands the thread back. Same `.await` in every runtime; the executor differs
  (`futures::executor::block_on` runs one future; Tokio runs many tasks plus an I/O reactor).
  Futures are lazy. `join!` vs sequential awaits.
- **Experiment (one thread, 30 simulated pages, ~7.6 s of total waiting):**
  10 loops with `tokio::time::sleep().await` = **0.94 s**; 1 loop = **7.74 s**;
  10 loops with `std::thread::sleep` (blocking) = **7.62 s**, peak in progress 1.
  Lesson: blocking calls silently destroy concurrency.
- **Three ways to cap at 10:** (1) ten permanent loops (chosen: structure guarantees the cap),
  (2) dispatcher + `tokio::sync::Semaphore` (acquire a permit *before* pulling),
  (3) `futures::StreamExt::for_each_concurrent(10, ...)` over a `stream::unfold` of pulls.
  Tokio provides the building blocks; it does not cap `spawn` by itself.
- Artifacts produced: a runnable `concurrency-demo` Rust project with all three options; a guide
  doc "Understanding Tokio Async in Rust (and Running Redis on Docker)" covering the mental model,
  spawn/join/JoinSet/select/timeout, Arc/Mutex/channels/Semaphore, blocking vs async, common
  compiler errors (`Send`, `'static`), a worker skeleton, Redis-on-Docker, redis-rs usage, and
  exercises.

## 4. Learning Redis, and the key design

- **Redis basics explained:** keys and types (string/int, list, set, hash); every command is
  atomic because Redis executes one command at a time; `MULTI`/`EXEC` runs a block atomically
  but cannot branch on intermediate results; pipelines save round trips but aren't atomic.
- **Claiming:** `SADD` returns 1 only for the first adder: a built-in "was I first?" check.
  `HSETNX` is the same for hash fields (used for submit dedup).
- **How workers find work across jobs:** read `jobs:active`, build the queue key names; the key
  returned by the pop tells the worker which job the URL belongs to. **Misconception corrected:**
  URLs are not "for" a specific worker; they belong to a job, and any worker can take any URL.
- **`seen` vs `pending`** (asked several times): `seen` is a permanent set of URLs ever claimed
  ("admissions register"); `pending` is a live number of claimed-but-unfinished URLs ("patients
  currently in the hospital"). pending = |seen| - finished. **Correction:** `meta` holds only base
  and status, not a running count.
- **Frontier / in flight** are about URLs inside one job, not jobs: frontier = queued (`LLEN`),
  in flight = taken but not reported (`pending - LLEN`), crawled = finished. Jobs are only
  running or done.
- A full worked example on `test-site/` was traced command by command, with every result:
  7 URLs, 6 files (html 5, svg 1), 88 words, 1 broken; a race where two pages discover
  `/docs/intro.html` and `SADD` lets exactly one claim it; `pending` never reaching 0 while a
  page was in flight even though the queue was empty; `/` and `/index.html` counted as two
  files (documented decision).
- **Bit arrays for dedup (student's idea) rejected:** mapping URLs to bits needs hashing ->
  collisions -> Bloom filter -> false positives skip real pages; which URL collides depends on
  insertion order, so results could differ with N; savings are tiny (a few thousand URLs ~ 1 MB);
  not faster (network round trip dominates). Exact alternative if memory mattered: a set of
  128-bit URL hashes.

## 5. Modularity

The student asked for a more modular design following coding principles. Result (now §8 of
`DESIGN.md`): functional core / imperative shell (pure `domain/` module with all rules, unit-tested),
`store/` as the only code that knows Redis, thin `commands/`, single responsibility, newtypes and
enums instead of strings and booleans, config in one place. Traits for every component were
deliberately **not** used (async-trait `Send` complications for a beginner, one implementation
each; YAGNI).

## 6. Global queue vs per-job queues, and fairness

- **Question:** how do N nodes pull properly if there's a queue per job, e.g. 10 jobs x 20 links?
  Answer: `BLPOP` (and later `take.lua`) accepts many keys and takes from the first non-empty one.
- **Student proposed** a global queue plus a per-job frontier counter for fairness. Analysis:
  global FIFO is fair to *URLs*, not *jobs*: a small job submitted behind a big job's 2,000 queued
  URLs waits ~13 s for its first page and again at every level. Memory is about the same either
  way. A per-job counter would be briefly off by one around each pop (pop and decrement can't be
  atomic with `BLPOP`). **Decision: per-job queues.**
- **"Isn't rotation fixed? How is it fair?"** Plain rotation is biased: when a job's queue is
  empty, its turns all go to the same neighbor (with A, B empty, C: C gets 2/3). **Decision:
  shuffle the keys on every take** (`fastrand::shuffle`); `rand`'s thread RNG isn't `Send`.
- Weighted, priority, and automatic weights (smallest backlog first, server speed, error-rate
  backoff, HTML-first via sorted sets and `BZPOPMIN`, shallow-first, popular-first) were explained
  with examples. None required. Weights change speed and order, never results. HTML-first is the
  most useful optional optimization (only HTML reveals links, so it reduces idle moments).

## 7. Idle moments

**Question:** if the queue is empty and one page is still in flight, do other nodes stop?
Answer: they wait (blocked pops, or polling in the final design) and are woken as soon as new
links are pushed. Narrow moments (e.g. the start of a crawl) are inherent to crawling: no design
can fetch pages whose links aren't known yet. They cost idle capacity, never correctness, and
other jobs fill the gaps.

## 8. Lua: introduced, dropped, then reinstated

1. Lua scripts were first proposed for submit and finish (all-or-nothing updates).
2. **Student: "Not sure why we need Lua."** (The spec's tips don't mention it.) Analysis showed
   that **without crashes**, plain commands + `MULTI` + ordering are correct: claim children in a
   pipeline of `SADD`s (round trip 1), then one `MULTI` with `INCRBY pending`, `RPUSH`, stats, and
   `DECR` last (round trip 2). The parent stays counted in `pending` during the gap, so termination
   holds. Lua was dropped.
3. **Crash survival brought it back.** If a node dies between round trip 1 and 2, its claimed
   children are in `seen` but never queued or counted. Everyone else's `SADD` returns 0 and trusts
   the claim, so the pages are never crawled, and the job still "finishes" with wrong stats.
   (Analogy: a table booked by someone who never shows up.) Traced step by step for the student.
4. **"A script is sequential too, why is it safe?"** Sequential isn't the problem; *who drives the
   steps* is. With round trips, the worker drives them and can die between messages. A script is
   one message; Redis runs it to completion as one command, uninterruptible by the caller dying and
   not interleaved with other clients. Caveats: no rollback on runtime errors; Redis crashes are
   out of scope.
5. **Other fixes considered:** `WATCH` + `MULTI` (correct, but constant aborts because every worker
   writes `seen`); an idempotent design (push all links unconditionally, dedupe when taking, store
   per-URL results with `HSET`, compute stats at the end: no claims, so no half-done claims;
   cost: duplicate queue entries); write-ahead intents (building your own transactions);
   Redis Functions (same as Lua). **Decision: Lua scripts.**

## 9. Crash recovery options

Explained in detail with real Redis 7 outputs:

- **Redis Streams:** append-only log; entries have IDs; consumer groups share entries like a queue;
  the Pending Entries List (PEL) tracks who holds what until `XACK`; `XAUTOCLAIM` reassigns entries
  idle too long. Streams would replace the job queues, and the PEL would replace processing lists.
  Cons: new concepts, groups per job, entries must be trimmed, idle-time detection can mistake a
  slow page for a dead node, multi-stream reads return several entries at once.
- **Lists + `BLMOVE` + heartbeats:** move a taken URL into a per-loop processing list (0 or 1 items,
  never growing); each node refreshes a heartbeat key with a TTL (Redis deletes it automatically if
  the node dies; that's the only part Redis does on its own); a reaper on every node requeues the
  processing lists of nodes without a heartbeat. Heartbeats distinguish a dead node from a slow request.
  Caveat: `BLMOVE` has a single source, so with per-job queues taking needs a script and polling.
- **Per-URL tracking** (`url -> queued/inflight/done/broken`): explained as per-URL detail vs our
  aggregate keys; useful for debugging and recovery; costs extra writes and scans. Not chosen.
- **Idempotent stats:** needed only with crash recovery, because a requeued URL from a slow-but-alive
  ("zombie") node can be reported twice; a double `DECR` could even end a job early. Fetching becomes
  at-least-once; counting must stay exactly-once.
- **Work partitioning by URL hash** (each node owns a slice; local dedup): rejected (fixed N,
  imbalance, a dead node's slice stalls).
- **Breadth-first rounds** (levels with barriers, mentioned in the spec): simpler termination, but
  every barrier waits for the slowest page. Not chosen.
- Others: job ID = hash of URL (automatic submit dedup); pub/sub for `status -f`; caching
  `jobs:active` with pub/sub invalidation. Not chosen.

**Decision (student's choice): Lists + processing lists + heartbeats + reapers**, with Lua scripts
`submit`, `take` (non-blocking, multi-queue move with polling backoff; resumes a held item),
`finish` (idempotent via `job:{id}:finished`; claims, stats and `DECR` in one step), and `requeue`.

## 10. Spec review findings

- A "file" includes resources linked by `src`/`href` (CSS, JS, images): the spec's example output
  counts css/js/png. Link extraction therefore covers `a, area, link` (href) and
  `img, script, iframe, source, embed, audio, video` (src). The early scaffold only used `<a href>`.
- `crawl submit` takes several URLs; each becomes its own job.
- The spec's tips: share one `reqwest::Client`; avoid `blocking`; HEAD is available; `redis` with
  `tokio-comp` and a cheap-to-clone `MultiplexedConnection`; optional `connection-manager`; pub/sub
  possible for `status -f`; `scraper::Html` isn't `Send`; `clap` for subcommands.
- Hand-in: private repo, tag `1.0.0`, compose or `docker run`, README sections, live demo.

## 11. Tooling and artifacts produced

- Claude Code was recommended for implementation; `CLAUDE.md` at the repo root gives it context.
- An early project scaffold (`crawl.zip`) was created and compiled: clap subcommands
  (`node`, `submit`, `status -f`, `stats`, `ping`), `keys.rs`, `WebStats` with Display, a node with
  10 loops, fetch/parse skeletons, `docker-compose.yml`, `test-site/`. It **predates the final
  design** and must be restructured per `DESIGN.md` §8.2.
- The four Lua scripts in `DESIGN.md` §7 were tested against Redis 7.0 (`redis-cli --eval`) with a
  simulated crash, reaper recovery, and a zombie duplicate report; results in `DESIGN.md` §12.
  The test harness is `scripts/test_scripts.sh`.
- Note: the sandbox used to compile the scaffold had an old Rust (1.75), so dependency versions
  were pinned there only; the student's machine should use current stable Rust and normal versions.

## 12. Implementation and what testing found

- The student asked Claude to write the complete implementation instead of leaving TODOs.
  It follows `DESIGN.md`; parsing moved into `domain/html.rs` (it's pure), and the node got a
  `-q` flag and graceful Ctrl+C shutdown (stop taking, finish in-flight URLs, leave the cluster).
- Tests: 18 unit tests; 33 script checks; an end-to-end suite (`scripts/e2e_test.sh`) covering
  1 vs 3 nodes, two jobs plus a resubmission, a richer site (`test-site/extra/`: CSS/JS/images,
  a redirect, query strings, broken and outside links), and a 200-page site behind a slow
  server with `kill -9` and freeze/resume tests. The hand-counted 88 words were confirmed.
- **Bug found by the zombie test, and fixed:** a node frozen for 16 s woke up with requests
  that had timed out and tried to report good pages as broken. The `finished` set only ignores
  the *second* report, so if the zombie's wrong result had arrived first it would have won.
  Fix: **ownership check** in `finish.lua`: a report is accepted only if the URL is still in
  the reporting loop's processing list. The reaper removes it from a dead node's list, so a
  zombie's late report is rejected (-2). This is the same idea as a "lease" or "fencing token"
  in distributed systems. Verified: zombie held 9 URLs, all recovered, all 9 late reports rejected,
  stats identical to the clean run.

### Second finding, on the student's Mac: temporary failures must be retried
- On macOS, the big-site tests returned different wrong totals on each run (219, 215, 221
  files). Small sites passed. Missing files were whole pages plus images linked only from them,
  so some fetches had failed and those pages were counted as broken.
- Likely cause: Python's test server queues only 5 waiting connections; with 30 loops
  connecting at once, macOS refuses the extras (Linux holds them), so the crawler saw failures.
- Real weakness exposed: one temporary failure made a page permanently "broken", so results
  depended on luck, breaking "same answer for any N". Fix: retry network errors, timeouts, 5xx
  and 429 up to 3 times with a growing pause; permanent errors like 404 aren't retried. The test
  server now queues 256 connections, and a new "flaky" mode (every URL fails once with 503)
  proves the retries work. Verified that this test fails with retries turned off (0 files).
- Also fixed earlier: the e2e script used `timeout`, which macOS doesn't have.

### Third finding: nodes didn't survive a Redis restart
- The student noticed that after restarting Redis, nodes never reconnected. Cause: the
  `MultiplexedConnection` never reconnects, and commands in flight when Redis died could wait
  for a reply forever (no timeout by default).
- First attempt, `ConnectionManager`, failed the test: it only reconnects after an error, and by
  default has no timeouts, so the hung commands never produced one. Reading its source also
  showed that timeouts are classified "retry immediately", not "reconnect".
- Fix: 5 s response / 2 s connect timeouts on every connection, and the heartbeat task acts as a
  health check: when a heartbeat fails, it opens a fresh connection shared by all of the node's
  loops. Tested with Redis down for 2, 6 and 15 s mid-crawl: all recover with correct stats.
- Also, at the student's request: fewer round trips (job list cached for 1 s per loop), explicit
  HTTP keep-alive settings, and an HTTP/1.1 test server (Python's default HTTP/1.0 closes every
  connection, so keep-alive couldn't be observed).

## 13. Open items and reminders

- Check the course's policy on AI-generated code; the student must be able to explain every part.
- Read and understand the code before the demo; be ready to explain the ownership check.
- Run `cargo clippy` locally (the sandbox used for writing the code had no clippy).
