# crawl: design document

Distributed web crawler for **Mini 1** (spec: https://goms.cs.muzoo.io/projects/mini1.html).

**What this file is for.** This is the blueprint: it describes *what* to build, precisely
enough to implement without guessing. It is the source of truth for the implementation.
`CONVERSATION_LOG.md` explains *why* each decision was made and what was rejected.
If the two ever disagree, this file wins.

**Status.** Implemented. It includes the optional extra challenge (a node can be killed
mid-crawl; every page is still counted exactly once). The Lua scripts in section 7 are tested
by `scripts/test_scripts.sh`, the Rust rules by `cargo test`, and the whole system (1 vs 3
nodes, two jobs, `kill -9`, and a frozen-then-resumed node) by `scripts/e2e_test.sh`.
One change was made during implementation: `finish.lua` now checks **ownership** (§7.3, §11).

Contents:
1. Glossary
2. Requirements and compliance map
3. Failure model and guarantees
4. Architecture
5. Redis primer: the commands this design uses
6. Redis keys: purpose, lifecycle, invariants
7. Lua scripts (full code)
8. Rust components, types and concurrency model
9. Flows, step by step
10. Domain rules (normalization, extensions, words, links)
11. Correctness argument
12. Worked example (tested trace)
13. Fairness and scheduling
14. Configuration
15. Testing plan
16. README outline
17. Hand-in checklist
18. Known limitations and possible extensions

---

## 1. Glossary

| Term | Meaning |
| --- | --- |
| **Job** | One crawl, created by `crawl submit <url>`. The URL is both the start point and the base path. Identified by a job ID like `0001`. |
| **Base path** | The submitted URL (normalized). Only URLs starting with it are crawled. |
| **Node** | One running `crawl node` process. N nodes form the cluster. Each has a random node ID. |
| **Worker loop** | One of the 10 Tokio tasks inside a node. Each handles one URL at a time. |
| **Claim** | Winning `SADD job:{id}:seen <url>` (it returned 1). The claimer must queue the URL. Exactly one claim per URL per job. |
| **Queue / frontier** | `job:{id}:queue`: claimed URLs waiting to be fetched. "Frontier" = its length. |
| **Take** | A worker loop moving one URL from a job queue into its own processing list (`take.lua`). |
| **Processing list** | `proc:{node}:{loop}`: the 0 or 1 URL a worker loop is holding right now, kept in Redis so it survives a crash. |
| **In flight** | URLs taken but not yet reported (including ones held by a dead node, until requeued). |
| **Report / finish** | Telling Redis a URL is done (`finish.lua`): stats, children, decrement `pending`. |
| **Pending** | `job:{id}:pending`: claimed but not finished = queued + in flight. 0 means the job is done. |
| **Heartbeat** | `node:{nid}:alive`, a key that expires after 10 s unless the node refreshes it every 3 s. |
| **Reaper** | A background task on every node that requeues the processing lists of dead nodes. |
| **Zombie** | A node that froze long enough to be considered dead, then woke up and kept working. |
| **Idempotent** | Doing it twice has the same effect as doing it once (reporting a URL is idempotent). |
| **Atomic** | Runs as one indivisible step: no other client sees a half-done state, and it can't stop halfway. |

## 2. Requirements and compliance map

Paraphrased from the spec. Each requirement lists where this document addresses it.

| # | Requirement | Where |
| --- | --- | --- |
| R1 | Rust, async on Tokio; `reqwest`, `redis` (`tokio-comp`), `scraper`, `clap` suggested | §8 |
| R2 | `crawl node` joins the cluster; told Redis's address; run N >= 1 times, possibly on different machines; nodes on the host, Redis in Docker, reached by IP | §6, §9.2, §14 |
| R3 | `crawl submit <url>...`: one job per URL; print job IDs; return immediately; a URL already submitted prints the existing job's ID | §7.1, §9.1 |
| R4 | `crawl status <job>`: pages crawled, frontier, in flight, done or not | §9.7 |
| R5 | `crawl status -f <job>`: follow until done, then exit (polling allowed) | §9.7 |
| R6 | `crawl stats <job>`: `WebStats` available as soon as the job is done; persists until Redis is cleared; say so if not done | §9.8 |
| R7 | CLI talks only to Redis | §4 |
| R8 | Each page crawled once across the whole cluster | §7, §11 |
| R9 | Several jobs at once without interfering | §6, §13 |
| R10 | Parallel within a node, at most 10 requests in flight | §8.4 |
| R11 | Cluster detects completion on its own | §7.3, §11 |
| R12 | Same `WebStats` for any N | §11 |
| R13 | A file = any existing URL under the base path that is linked; non-HTML included; HEAD is enough for non-HTML; drop fragments | §10 |
| R14 | HTML decided by `Content-Type`; words: lowercase, start with a-z, excluding tags, attributes, comments | §10 |
| R15 | Extensions lowercase; no extension = `html`; `.jpg` != `.jpeg` | §10 |
| R16 | Stay strictly under the base path | §10 |
| R17 | Hand-in: private GitHub repo, good commits/branching, tag `1.0.0`; code + Cargo; `docker run` or compose for Redis; README (coordination, progress, termination, run instructions); live demo | §16, §17 |
| R18 | Extra challenge: survive a node killed mid-crawl; every page counted exactly once | §3, §7, §11, §12 |

```rust
// From the spec.
pub struct WebStats {
    pub num_files: usize,                    // the total number of (unique) files found
    pub num_exts: usize,                     // the total number of unique extensions
    pub ext_counts: HashMap<String, usize>,  // the total number of files for each extension
    pub total_word_count: u64,               // words in all HTML files combined
}
```

## 3. Failure model and guarantees

**What can fail:**
- A crawler node can be killed at any moment (`kill -9`), including mid-request and
  mid-report, or frozen for a while (`kill -STOP`, laptop sleep) and then resume.
- The CLI can be killed at any moment.
- **Redis does not fail** (out of scope). Websites can be slow, broken, or return errors.

**What we guarantee:**
- **No URL is lost.** A URL held by a node that dies is put back and crawled by someone else.
- **Every URL is counted exactly once.** Fetching is at-least-once (a URL may occasionally be
  fetched twice after a crash or freeze); counting is exactly-once.
- **Correct termination.** A job is marked done exactly once, and only when all its claimed
  URLs are finished.
- **Same `WebStats` for any N and any crash pattern.**

## 4. Architecture

```
          crawl submit / status / stats            (short-lived CLI commands)
                         |
                         v
   +---------------------------------------------------+
   |                 Redis (Docker)                     |
   |  jobs:*   job:{id}:*   nodes   node:*:alive   proc:*|
   +---------------------------------------------------+
        ^              ^               ^
        |              |               |       (every arrow: Redis commands / Lua scripts)
   crawl node A    crawl node B    crawl node C    (N long-running processes, any machines)
   10 loops        10 loops        10 loops
   heartbeat       heartbeat       heartbeat
   reaper          reaper          reaper
        |              |               |
        +--------------+---------------+-----> target website (HEAD / GET)
```

- **Three kinds of process**: the CLI, N nodes, one Redis. Nodes never talk to each other
  or to the CLI; everything goes through Redis.
- **Pull model**: one queue per job. A worker loop takes the next URL itself when it is free.
  There is no scheduler process.
- **Inside a node**: single-threaded Tokio runtime with 10 worker loops, 1 heartbeat task,
  1 reaper task; one shared `reqwest::Client`; one shared `MultiplexedConnection`.
- **Atomicity**: every multi-step update that other workers rely on is one Lua script.
  Read-only snapshots use `MULTI`/`EXEC`.
- **Fairness**: on every take, the active jobs' queue keys are shuffled.
- **Crash recovery**: a taken URL lives in Redis (processing list) until reported; heartbeats
  reveal dead nodes; reapers (on every node) requeue their URLs; reporting is idempotent.

## 5. Redis primer: the commands this design uses

Redis is a server holding named values (**keys**) in memory. Each key has a type. Clients send
commands over the network. **Redis executes commands one at a time**, so every single command
is atomic. A **Lua script** (`EVAL`/`EVALSHA`, via `redis::Script` in Rust) is also executed
as one command: no other client's command can run in the middle of it, and once Redis has
received it, it runs to the end even if the client dies. Scripts do **not** roll back if a
command inside them errors, so they must be written and tested carefully.

| Command | Type | What it does | Returns |
| --- | --- | --- | --- |
| `GET k` / `SET k v` | string | Read / write | value or nil / OK |
| `SET k v EX 10` | string | Write with expiry: Redis deletes the key after 10 s | OK |
| `EXISTS k` | any | Does the key exist? | 1 / 0 |
| `INCR k`, `INCRBY k n`, `DECR k` | int | Atomic add/subtract (missing key counts as 0) | the new value |
| `RPUSH k v...` | list | Append to the end | new length |
| `LPOP k` | list | Remove and return the first item | item or nil |
| `LINDEX k 0` | list | Read the first item without removing it | item or nil |
| `LREM k 0 v` | list | Remove all occurrences of v | number removed |
| `LRANGE k 0 -1`, `LLEN k` | list | Read all / length | items / number |
| `SADD k m` | set | Add member if absent | **1 if added, 0 if already there** |
| `SISMEMBER k m`, `SCARD k`, `SMEMBERS k`, `SREM k m` | set | Test / size / all / remove | 1 or 0 / number / members / 1 or 0 |
| `HSET k f v ...`, `HGET k f`, `HGETALL k` | hash | Write / read field / read all | count / value / pairs |
| `HINCRBY k f n`, `HLEN k`, `HDEL k f` | hash | Add to a field / number of fields / delete field | new value / number / 1 or 0 |

Empty lists, sets and hashes are deleted automatically by Redis.

## 6. Redis keys: purpose, lifecycle, invariants

**Formats.** Job IDs come from `INCR jobs:next_id`, formatted as at least 4 lowercase hex
digits (`format!("{:04x}", n)` -> `0001`, `00ff`, `1000`). Node IDs are random per process
start (`format!("{:08x}", fastrand::u32(..))`), so a restarted node never reuses a dead node's
keys. Processing-list items are `"<job_id> <url>"`; normalized URLs never contain spaces (the
`url` crate percent-encodes them), so splitting on the first space is safe.

### 6.1 Global keys

| Key | Type | Purpose | Created / written by | Read by | Removed |
| --- | --- | --- | --- | --- | --- |
| `jobs:next_id` | int | Hands out job numbers | CLI submit (`INCR`) | CLI submit | Never (until `FLUSHALL`) |
| `jobs:by_url` | hash url -> id | "Was this URL already submitted? As which job?" | `submit.lua` | `submit.lua` | Never |
| `jobs:active` | set of ids | "Which jobs should workers take from?" | `submit.lua` adds | Worker loops each iteration | `finish.lua` when pending hits 0 |
| `nodes` | hash nid -> loop count | "Which nodes must the reaper watch, and how many loops do they have?" | Node startup, heartbeat (re-register) | Reapers | Reaper after recovering a dead node; node on graceful shutdown |
| `node:{nid}:alive` | string with TTL | "Is node nid alive?" | Heartbeat every 3 s (`SET ... EX 10`) | Reapers (`EXISTS`) | Expires 10 s after the last refresh; graceful shutdown deletes it |
| `proc:{nid}:{loop}` | list, 0 or 1 items | "What is this loop holding right now?" | `take.lua` | Reapers, `take.lua` (resume) | `finish.lua` (`LREM`), `requeue.lua` (`LPOP`) |

### 6.2 Per-job keys

| Key | Type | Purpose | Written by | Read by | Lifetime |
| --- | --- | --- | --- | --- | --- |
| `job:{id}:meta` | hash | `base`, `status` (`running`/`done`), `submitted_at`, `finished_at` | `submit.lua`; `finish.lua` (done) | Workers (`base`, cached), CLI | Kept (stats must persist) |
| `job:{id}:queue` | list of urls | Frontier: claimed URLs waiting | `submit.lua`, `finish.lua`, `requeue.lua` | `take.lua`, CLI (`LLEN`) | Disappears when empty |
| `job:{id}:seen` | set of urls | Every URL ever claimed; claiming = `SADD` returns 1 | `submit.lua`, `finish.lua` | Same scripts (via `SADD`) | Kept |
| `job:{id}:finished` | set of urls | Every URL already reported; makes reporting idempotent | `finish.lua` | `finish.lua`, `requeue.lua` | Kept |
| `job:{id}:pending` | int | Claimed but unfinished (queued + in flight) | `submit.lua` (=1), `finish.lua` (`INCRBY`, `DECR`) | `finish.lua`, CLI | Kept (ends at 0) |
| `job:{id}:stats` | hash | `crawled`, `num_files`, `broken`, `redirects`, `total_word_count` | `finish.lua` | CLI | Kept |
| `job:{id}:ext` | hash ext -> count | Files per extension; `num_exts` = `HLEN` | `finish.lua` | CLI | Kept |

How similar-looking keys differ:
- `seen` vs `finished`: `seen` = claimed (work started or waiting); `finished` = reported (work done).
- `queue` vs `pending`: `queue` = waiting only; `pending` = waiting + held/being processed.
- `jobs:by_url` vs `meta.base`: URL -> ID (dedup at submit) vs ID -> URL (filtering links).
- `jobs:active` vs `meta.status`: the short list workers scan vs the job's state for the CLI.

### 6.3 Invariants (every script preserves them)

1. `pending == SCARD seen - SCARD finished`.
2. Every URL in `seen` but not in `finished` is in exactly one place: the job queue, or one
   processing list (or, briefly, a frozen zombie also works on it; see §11).
3. `stats.crawled == SCARD finished`.
4. A job is in `jobs:active` exactly while `pending > 0`; `meta.status == "done"` exactly when `pending == 0`.
5. Each processing list holds 0 or 1 items.


### 6.4 Deduplication: alternatives considered

**Chosen:** one `job:{id}:seen` set in Redis holding full URLs. It is exact, shared by every
node, and survives any node crash. Its cost is memory proportional to the number of URLs,
which is small for one site.

| Alternative | Idea | Why it was not chosen |
| --- | --- | --- |
| Bloom filter (shared, or one per worker) | Store a few bits per URL instead of the URL | It is approximate: it sometimes reports a new URL as seen, and that page is then never crawled. Stats would be slightly wrong, and wrong differently for different N, breaking R12. Its benefit is memory, which matters when crawling many sites, not one. |
| A local set per worker, with URLs routed by hash | `hash(url)` decides which worker owns a URL, so the owner's local set is enough | A dead worker's set is in its memory and is lost, and its slice of URLs stalls, which breaks crash recovery (R18). It needs a queue per node and routing instead of the pull model, and changing N remaps URLs. Consistent hashing with virtual nodes evens out URL counts, but not work: pages differ in cost, and an idle worker cannot take URLs from another worker's slice. |
| A local cache in front of `seen` | Each node remembers URLs it knows are seen and does not send them to Redis again | Tested and rejected. After Redis was taken down and brought back with its data cleared, a newly submitted job reused a job ID, and the node skipped its URLs because its in-memory cache still listed them as seen. Clearing the cache when the Redis connection drops would fix it, but the gain does not justify the extra state: children already travel in one batch per page (§7.3), so the cache saves no round trips. |
| Storing 128-bit hashes instead of URLs | `seen` holds a fixed-size hash of each URL; one `hash -> status` map could replace both `seen` and `finished` | Exact in practice (a collision among a billion URLs has a probability of about 1 in 10^21) and smaller than full URLs, but not needed at this scale. The sets would no longer be readable in `redis-cli`, and every node must use the same fixed hash function (Rust's default hasher is seeded per process). Listed as a possible extension (§18). |

A local set per worker **without** routing was not considered: two workers could both see
the same URL as new and both crawl and count it.

## 7. Lua scripts (full code)

Stored in `scripts/`, embedded with `include_str!`, wrapped in `redis::Script` (which uses
`EVALSHA` and loads the script automatically when needed). In Rust:
`Script::new(SRC).key(..).arg(..).invoke_async(&mut con).await`. A Lua `false` return
becomes Redis nil, which maps to `Option::None` in Rust.

Some scripts build per-job key names inside Lua (`'job:' .. id .. ':queue'`). That is fine on a
single, non-cluster Redis, which is what this project uses.

### 7.1 `submit.lua`

```lua
-- submit.lua: create a crawl job atomically, or return the existing one.
-- KEYS[1] = jobs:by_url        KEYS[2] = jobs:active
-- ARGV[1] = normalized url     ARGV[2] = candidate job id     ARGV[3] = now (unix seconds)
-- Returns {"new", id} or {"existing", id}.
local existing = redis.call('HGET', KEYS[1], ARGV[1])
if existing then
  return {'existing', existing}
end
local id = ARGV[2]
local p = 'job:' .. id .. ':'
redis.call('HSET', KEYS[1], ARGV[1], id)
redis.call('HSET', p .. 'meta', 'base', ARGV[1], 'status', 'running', 'submitted_at', ARGV[3])
redis.call('SADD', p .. 'seen', ARGV[1])
redis.call('SET', p .. 'pending', 1)
redis.call('RPUSH', p .. 'queue', ARGV[1])
redis.call('SADD', KEYS[2], id)
return {'new', id}
```

Why a script: if the CLI died between registering the URL and creating the job's keys,
the URL would point to a job that never runs. The CLI gets the candidate ID from
`INCR jobs:next_id` first; if the URL already exists, that number is simply never used.

### 7.2 `take.lua`

```lua
-- take.lua: atomically move one URL from the first non-empty job queue into this
-- worker loop's processing list. Non-blocking.
-- KEYS[1]    = proc:{node}:{loop}  (this loop's processing list, 0 or 1 items)
-- KEYS[2..n] = job:{id}:queue keys, already shuffled by the caller
-- ARGV[i]    = the job id belonging to KEYS[i + 1]
-- Returns {job_id, url}, or nil if every queue is empty.
local function split(item)
  local sp = string.find(item, ' ', 1, true)
  return {string.sub(item, 1, sp - 1), string.sub(item, sp + 1)}
end
-- If this loop still holds an item (its previous finish never ran), resume it.
local held = redis.call('LINDEX', KEYS[1], 0)
if held then
  return split(held)
end
for i = 2, #KEYS do
  local url = redis.call('LPOP', KEYS[i])
  if url then
    local id = ARGV[i - 1]
    redis.call('RPUSH', KEYS[1], id .. ' ' .. url)
    return {id, url}
  end
end
return false
```

Why a script: a plain `LPOP` would leave the URL only in the worker's memory (lost on a crash).
`BLMOVE` would keep it in Redis, but only accepts one source list, and we have one queue per
job. The script tries the shuffled queues in order and moves the first URL it finds, in one
round trip. Scripts cannot block, so the caller polls with backoff when it returns nil.
The "resume" branch means a loop that failed to report (e.g. a Redis error) simply retries
the same URL; reporting is idempotent, so this is safe.

### 7.3 `finish.lua`

```lua
-- finish.lua: report one URL, all or nothing, at most once.
-- KEYS: 1 proc list, 2 seen, 3 queue, 4 pending, 5 stats, 6 ext, 7 finished, 8 meta, 9 jobs:active
-- ARGV: 1 job id, 2 url, 3 outcome ("file" | "broken" | "redirect"), 4 extension,
--       5 word count, 6 now (unix seconds), 7.. child urls (normalized, under base, deduplicated)
-- Returns pending after this report (0 = job finished),
--   -2 if this loop no longer holds the URL (a reaper gave it to someone else), or
--   -1 if the URL was already reported.

-- 1. Ownership: only the loop that currently holds the URL may report it. If a reaper
--    moved it back to the queue (this node looked dead), the report is rejected, so a
--    late result from a frozen "zombie" node can never win against the real one.
if redis.call('LREM', KEYS[1], 0, ARGV[1] .. ' ' .. ARGV[2]) == 0 then
  return -2
end

-- 2. Count each URL once (a second line of defense).
if redis.call('SADD', KEYS[7], ARGV[2]) == 0 then
  return -1
end

-- 3. Claim children; queue and count the ones we won.
local won = {}
for i = 7, #ARGV do
  if redis.call('SADD', KEYS[2], ARGV[i]) == 1 then
    won[#won + 1] = ARGV[i]
  end
end
if #won > 0 then
  redis.call('INCRBY', KEYS[4], #won)
  for s = 1, #won, 1000 do                       -- push in chunks (Lua unpack limit)
    redis.call('RPUSH', KEYS[3], unpack(won, s, math.min(s + 999, #won)))
  end
end

-- 4. Stats for this URL.
redis.call('HINCRBY', KEYS[5], 'crawled', 1)
if ARGV[3] == 'file' then
  redis.call('HINCRBY', KEYS[5], 'num_files', 1)
  redis.call('HINCRBY', KEYS[6], ARGV[4], 1)
  redis.call('HINCRBY', KEYS[5], 'total_word_count', tonumber(ARGV[5]))
elseif ARGV[3] == 'broken' then
  redis.call('HINCRBY', KEYS[5], 'broken', 1)
elseif ARGV[3] == 'redirect' then
  redis.call('HINCRBY', KEYS[5], 'redirects', 1)
end

-- 5. This URL is finished. Children were added first, so 0 really means "nothing left".
local left = redis.call('DECR', KEYS[4])
if left == 0 then
  redis.call('HSET', KEYS[8], 'status', 'done', 'finished_at', ARGV[6])
  redis.call('SREM', KEYS[9], ARGV[1])
end
return left
```

Why the order matters:
- Step 1 first: only the current holder may report. If a reaper moved the URL back to the
  queue because this node looked dead, the node has lost the URL, and its late result is
  rejected (-2) before anything is counted. This was found by testing: a node frozen for
  16 s woke up with timed-out requests and tried to report good pages as **broken**. Without
  the ownership check, whichever report arrived first would win, so the stats could depend
  on timing.
- Step 2: a second report of a URL must not claim, count, or decrement again.
- Step 3 before step 5: children are counted in `pending` before the parent is subtracted,
  so `pending` cannot touch 0 while work remains.
- Step 4 before step 5: when a job becomes done, its stats are already complete.
- Why one script (not two round trips): with crashes possible, a node killed after claiming
  children (step 3's `SADD` = 1) but before queueing them would leave them claimed forever and
  never crawled; the job would still "finish" with wrong stats. One script cannot stop halfway.

### 7.4 `requeue.lua`

```lua
-- requeue.lua: move one item from a dead worker loop's processing list back to its job queue.
-- KEYS[1] = proc:{dead node}:{loop}
-- Returns the moved item "<job_id> <url>", or nil when the list is empty.
-- `pending` is untouched: the URL was never finished, so it is still counted.
local item = redis.call('LPOP', KEYS[1])
if not item then
  return false
end
local sp = string.find(item, ' ', 1, true)
local id = string.sub(item, 1, sp - 1)
local url = string.sub(item, sp + 1)
if redis.call('SISMEMBER', 'job:' .. id .. ':finished', url) == 0 then
  redis.call('RPUSH', 'job:' .. id .. ':queue', url)
end
return item
```

### 7.5 Why Lua scripts, and not the alternatives

The hardest operation is step 3 of `finish.lua`: `SADD seen <child>`, and **only if that
returned 1**, `INCRBY pending` and `RPUSH queue`. A multi-step update like this can break in
two ways:

- **Interleaving**: another worker's command runs between the steps and sees or changes a
  half-done state (e.g. `pending` reaching 0 while a claimed child is not yet counted).
- **Crash**: the worker dies between the steps (e.g. after `SADD`, before `RPUSH`), leaving
  a URL claimed forever and never crawled.

A Lua script prevents both (Redis runs it as one command, and it cannot stop halfway once
received), can branch on results in the middle, and costs one round trip.

| Alternative | How it works | Why it was not chosen |
| --- | --- | --- |
| `MULTI` / `EXEC` | Queues commands and runs them as one block | Atomic, and one round trip when pipelined, but the commands are only queued: no result is visible until `EXEC`, so "only if `SADD` returned 1" cannot be expressed. Used here only for read-only snapshots (§9.7). |
| `WATCH` + `MULTI` | Read, decide in Rust, then `EXEC`, which aborts if a watched key changed | Correct, but every loop on every node writes `seen`, so transactions would abort and retry constantly, more so with more nodes. At least two round trips per attempt. `WATCH` is per connection, so each loop would need its own connection instead of the shared `MultiplexedConnection`. |
| Distributed lock | `SET lock NX PX`, run the steps, release | About five round trips per operation, and every worker on a job waits for the same lock. It stops interleaving but not crashes: a node killed after `SADD` leaves the child claimed and never queued; the lock expires, the half-done state stays. A slow holder can also outlive the lock's expiry, and releasing safely needs a script anyway. |
| Idempotent design (no claims) | Push every link unchecked, drop duplicates when taking, store one result per URL, compute stats at the end | Needs no atomicity, since every step is safe to repeat. But the queue holds one copy of a URL for every page that links to it, stats are no longer live counters, and there is no exact `pending` count, so detecting completion needs a different mechanism. |

One idea from the idempotent design is kept: reporting a URL twice is harmless (`finished`
set, step 2 of `finish.lua`), which is what crash recovery relies on.

The cost of Lua: a second language to maintain, and no rollback if a script hits a runtime
error part-way (§5, §18), so the scripts are kept short and are tested by
`scripts/test_scripts.sh`.

Safe with several reapers at once: `LPOP` hands the item to exactly one of them.

## 8. Rust components, types and concurrency model

### 8.1 Crates

| Crate | Use | Notes |
| --- | --- | --- |
| `tokio` (`full`) | Runtime, tasks, timers, signals | `current_thread` runtime in nodes |
| `reqwest` | Async HTTP, HEAD and GET | `default-features = false`, features `rustls-tls`, `charset`, `http2`; never `blocking` |
| `redis` (`tokio-comp`) | Async Redis, `MultiplexedConnection`, `Script` | Optional `connection-manager` for auto-reconnect |
| `scraper` | HTML parsing | `Html` is not `Send`: parse in one synchronous function |
| `url` | Parsing, joining relative links, fragments | |
| `clap` (`derive`, `env`) | Subcommands, `-f`, `--redis` / `REDIS_URL` | |
| `anyhow` | Error handling with `?` | |
| `fastrand` | Shuffle, node IDs | `fastrand::shuffle` avoids holding a non-`Send` RNG across `.await` |

### 8.2 Modules and responsibilities

```
src/
  main.rs            parse args (cli.rs), build Config, RedisStore, HttpFetcher; dispatch
  cli.rs             clap definitions only
  config.rs          Config { redis_url, worker_loops, http_timeout, heartbeat, reaper, backoff }
  domain/            PURE: no async, no Redis, no HTTP. Fully unit-tested.
    job.rs           JobId newtype (validation, Display), JobStatus
    url_rules.rs     normalize(), resolve(), is_under_base(), extension()
    words.rs         count_words()
    html.rs          parse_html(body, page_url) -> ParsedPage { links, words }; synchronous
    page.rs          FetchOutcome, Outcome, PageReport::build()
    stats.rs         WebStats + Display (spec format)
  store/             The ONLY code that knows Redis commands, key names and scripts.
    keys.rs          key-name functions
    scripts.rs       include_str!("../../scripts/*.lua") wrapped in redis::Script
    redis_store.rs   RedisStore (cheap Clone): methods listed in 8.3
  crawler/
    fetch.rs         HttpFetcher: HEAD/GET -> FetchOutcome
    worker.rs        one worker loop
    heartbeat.rs     heartbeat task
    reaper.rs        reaper task
  commands/          thin glue: submit.rs, status.rs, stats.rs, node.rs, ping.rs
scripts/             submit.lua, take.lua, finish.lua, requeue.lua
```

Dependencies point downward: `commands -> crawler/store -> domain`. `domain` depends on nothing
in the crate.

### 8.3 Key types and signatures (sketch)

```rust
// domain
pub struct JobId(String);                                  // "0001"
pub enum FetchOutcome {
    Html { body: String },                                 // 2xx and HTML Content-Type
    File,                                                  // 2xx, not HTML
    Redirect { location: Option<String> },                 // 3xx
    Broken,                                                // 4xx, or 5xx/timeout/network error after retries
}
pub enum Outcome { File, Broken, Redirect }
pub struct PageReport {
    pub outcome: Outcome,
    pub extension: String,                                 // meaningful only for File
    pub words: u64,                                        // 0 unless HTML file
    pub children: Vec<String>,                             // normalized, under base, deduplicated
}
impl PageReport {
    pub fn build(page_url: &url::Url, base: &str, fetched: FetchOutcome) -> PageReport;
}
pub struct JobStatus { pub crawled: u64, pub frontier: u64, pub in_flight: u64,
                       pub files: u64, pub broken: u64, pub done: bool }

// store
pub enum SubmitResult { New(JobId), Existing(JobId) }
impl RedisStore {
    pub async fn connect(url: &str) -> anyhow::Result<Self>;
    pub async fn ping(&self) -> anyhow::Result<()>;
    pub async fn submit(&self, url: &str) -> anyhow::Result<SubmitResult>;
    pub async fn active_jobs(&self) -> anyhow::Result<Vec<JobId>>;
    pub async fn job_base(&self, job: &JobId) -> anyhow::Result<Option<String>>;
    pub async fn take(&self, proc_key: &str, jobs: &[JobId]) -> anyhow::Result<Option<(JobId, String)>>;
    pub async fn finish(&self, proc_key: &str, job: &JobId, url: &str, report: &PageReport) -> anyhow::Result<i64>;
    pub async fn register_node(&self, nid: &str, loops: usize, ttl_secs: u64) -> anyhow::Result<()>;
    pub async fn deregister_node(&self, nid: &str) -> anyhow::Result<()>;
    pub async fn dead_nodes(&self, me: &str) -> anyhow::Result<Vec<(String, usize)>>;
    pub async fn requeue(&self, proc_key: &str) -> anyhow::Result<Option<String>>;
    pub async fn status(&self, job: &JobId) -> anyhow::Result<Option<JobStatus>>;
    pub async fn stats(&self, job: &JobId) -> anyhow::Result<StatsResult>; // NoSuchJob | Running | Done(WebStats)
}

// crawler
impl HttpFetcher { pub async fn fetch(&self, url: &str) -> FetchOutcome; }
pub fn parse_html(body: &str, page_url: &url::Url) -> ParsedPage;    // ParsedPage { links: Vec<String>, words: u64 }
```

### 8.4 Concurrency model inside a node

- `#[tokio::main(flavor = "current_thread")]`: one OS thread. Tokio switches between tasks at
  every `.await` that has to wait; this is where concurrency comes from.
- 10 worker loops spawned into a `JoinSet`; each handles exactly one URL at a time, so at most
  10 requests are in flight per node by construction (no semaphore needed).
- 1 heartbeat task and 1 reaper task, also spawned.
- Shared: one `reqwest::Client` (clone per loop), one `RedisStore` wrapping a
  `MultiplexedConnection` (clone per loop). No blocking Redis commands are used, so no
  dedicated connections are needed.
- Never block the thread: no `std::thread::sleep`, no `reqwest::blocking`, no sync Redis calls.
  Parsing is synchronous but short; `scraper::Html` is created and dropped inside `parse_html`.
  If parsing ever blocked the thread for more than 10 s, the heartbeat would be missed and the
  node's URL might be requeued; reporting is idempotent, so the result stays correct.

### 8.5 Errors and logging

- CLI commands return `anyhow::Result`; print a clear message and exit with code 1 on failure
  (e.g. Redis unreachable, unknown job).
- Worker loops never die on errors: HTTP problems become `FetchOutcome::Broken`; Redis errors
  are logged and retried with a 1 s backoff. If `finish` fails, the URL stays in the processing
  list and the next `take` resumes it (idempotent).
- Logging: `eprintln!` with a prefix like `[node 3fa9c210 loop 4]` is enough. Log takes, finishes
  (with outcome), duplicates (`finish` returned -1), job completion (returned 0), and every
  requeue done by the reaper.

## 9. Flows, step by step

### 9.1 `crawl submit <url>...`
For each URL argument:
1. Normalize (§10.1). If invalid or not http/https: print `error: <url>: <reason>`, continue.
2. `INCR jobs:next_id` -> candidate ID (format `{:04x}`).
3. `submit.lua` with the normalized URL, candidate ID, current unix time.
4. Print `job <id>  <url>` (append `(already submitted)` for an existing job).
Exit 0 if every URL was handled, 1 if any was invalid.

### 9.2 `crawl node`
1. Generate the node ID; print `node <nid> started with 10 worker loops`.
2. `register_node`: `HSET nodes <nid> 10`, `SET node:<nid>:alive 1 EX 10`.
3. Spawn the heartbeat (§9.4), the reaper (§9.5) and 10 worker loops (§9.3).
4. Wait for Ctrl+C (`tokio::signal::ctrl_c`). Graceful shutdown: stop taking new URLs, let
   in-flight URLs finish (or requeue this node's own processing lists), then `deregister_node`
   (`HDEL nodes <nid>`, `DEL node:<nid>:alive`). Without graceful shutdown, the reaper recovers
   the work after the heartbeat expires; graceful shutdown just makes it immediate.

### 9.3 Worker loop (`proc_key = proc:<nid>:<loop>`)
```
backoff = 50ms
loop:
  jobs = SMEMBERS jobs:active
  if jobs empty: sleep(backoff); backoff = min(backoff*2, 500ms); continue
  shuffle(jobs)
  match take(proc_key, jobs):
    None: sleep(backoff); backoff = min(backoff*2, 500ms); continue
    Some((job, url)):
      backoff = 50ms
      base = cached HGET job:{job}:meta base
      fetched = fetcher.fetch(url).await          // HTTP, see 9.6
      report = PageReport::build(url, base, fetched)   // pure, see 10
      left = finish(proc_key, job, url, report)    // retry with 1 s backoff on Redis error
      log outcome; if left == 0: log "job <id> finished"; if left == -1: log duplicate
```

### 9.4 Heartbeat
Every 3 s: `SET node:<nid>:alive 1 EX 10` and `HSET nodes <nid> 10` (re-registers the node if a
reaper removed it while it was frozen). Runs as its own task, so a slow HTTP request never delays it.

### 9.5 Reaper
Every 5 s: `HGETALL nodes`. For each `(nid, loops)` with `nid != me` and `EXISTS node:<nid>:alive == 0`:
call `requeue.lua` on `proc:<nid>:0` .. `proc:<nid>:<loops-1>` until each returns nil; log each
moved item; then `HDEL nodes <nid>`. Several reapers may handle the same dead node; this is safe.

### 9.6 Fetch decision table
| HEAD result | Action | `FetchOutcome` |
| --- | --- | --- |
| 2xx, HTML `Content-Type` | GET, read body | `Html { body }` (GET failing -> `Broken`) |
| 2xx, other `Content-Type` | nothing more | `File` |
| 3xx | read `Location` header | `Redirect { location }` |
| 405 or 501 (HEAD not supported) | GET instead, then apply the rows above | as above |
| 5xx or 429 | retry (up to 3 attempts, pausing 250 ms then 500 ms) | after the last attempt: `Broken` |
| other 4xx | nothing more | `Broken` |
| timeout / connection error | retry, as for 5xx | after the last attempt: `Broken` |
Client: `redirect::Policy::none()`, timeout 10 s, one shared `reqwest::Client`.

### 9.7 `crawl status <job>` and `crawl status -f <job>`
One `MULTI`: `HGET meta status`, `LLEN queue`, `GET pending`, `HGETALL stats`.
- No `meta` -> `no such job <id>`, exit 1.
- frontier = `LLEN`; in flight = `pending - frontier`.
- Output: `job 0001  crawled 7   frontier 0   in flight 0   files 6   broken 1   done`
  (last word `running` or `done`).
- `-f`: print that line every 500 ms; exit 0 after printing a line that says `done`.

### 9.7b `crawl status` without a job id (added later)
Lists every job, oldest first: reads `jobs:by_url` (every submitted URL and its job id), then
prints one status row per job. With `-f`, refreshes the table every 500 ms until every job is done.

### 9.8 `crawl stats <job>`
- No `meta` -> `no such job <id>`, exit 1.
- `status != done` -> `job <id> is still running (crawled N so far)`, exit 0.
- Else `HGETALL stats` + `HGETALL ext` -> `WebStats` (`num_exts` = number of ext fields) and print:
```
files: 6   extensions: 2   words: 88
  html 5   svg 1
```
Extensions sorted by count descending, then name ascending (stable output for comparing runs).

### 9.9 `crawl ping`
`PING` Redis; prints `redis replied PONG`. Useful for setup checks (not required by the spec).

## 10. Domain rules

### 10.1 Normalization (`url_rules::normalize`)
1. Parse with `url::Url` (relative links: `page_url.join(href)`).
2. Keep only `http` and `https`; anything else (`mailto:`, `javascript:`, `data:`) is ignored.
3. Remove the fragment (`set_fragment(None)`).
4. Everything else is left to the `url` crate: lowercase scheme and host, default port removed,
   `.`/`..` resolved, spaces percent-encoded. Query strings are kept. No trailing-slash or
   `index.html` rewriting.

| Input (on page `http://h/docs/a.html`) | Result |
| --- | --- |
| `b.html#intro` | `http://h/docs/b.html` |
| `../index.html` | `http://h/index.html` |
| `/x?page=2` | `http://h/x?page=2` |
| `HTTP://H:80/Y` | `http://h/Y` |
| `mailto:a@b.c` | ignored |

### 10.2 Base path (`url_rules::is_under_base`)
`normalized_url.as_str().starts_with(base)` where `base` is the job's normalized submitted URL.
This is literally what the spec says ("begin with this base path"). Note: a base without a
trailing slash (`http://h/docs`) also matches `http://h/docs-old`; document this in the README.

### 10.3 Extension (`url_rules::extension`)
Take the URL's path, then its last segment (after the last `/`). If it contains a `.` with at
least one character after it, the extension is the text after the last `.`, lowercased.
Otherwise `html`. Query strings never count.

| URL | Extension |
| --- | --- |
| `http://h/` | `html` |
| `http://h/docs/` | `html` |
| `http://h/a.HTML` | `html` |
| `http://h/img/Photo.JPG` | `jpg` |
| `http://h/img/photo.jpeg` | `jpeg` |
| `http://h/archive.tar.gz` | `gz` |
| `http://h/README` | `html` |
| `http://h/file.` | `html` |
| `http://h/page.php?id=3` | `php` |

The extension is decided by the URL, not by `Content-Type` (a PNG served at `/logo` counts as
`html`, but is not parsed). Mention this in the README.

### 10.4 HTML detection
`Content-Type` (from HEAD, or GET when HEAD is unsupported) starts with `text/html` or
`application/xhtml+xml`, case-insensitive, ignoring parameters like `; charset=utf-8`.

### 10.5 Links (`parse::parse_html`)
Collect `href` from `<a>`, `<area>`, `<link>` and `src` from `<img>`, `<script>`, `<iframe>`,
`<source>`, `<embed>`, `<audio>`, `<video>`. Resolve each against the page URL (or the page's
`<base href>`, if present), normalize,
keep those under the base path, deduplicate within the page, and never include the page itself.
Reason: the spec counts every linked file (its example has css, js, png), and those are
usually referenced by `<link href>`, `<script src>`, `<img src>`, not `<a href>`.

### 10.6 Word counting (`words::count_words`)
- Input: the document's text nodes, skipping text inside `<script>`, `<style>`, `<noscript>`.
  Using text nodes automatically excludes tags, attributes and comments. `<title>` text counts.
- Lowercase, split on every character that is not alphanumeric, count tokens whose first
  character is `a`-`z`.
- Examples: `"Hello, World!"` -> 2; `"An introduction with 123 numbers"` -> 4 (`123` excluded);
  `"don't"` -> 2 (`don`, `t`); `"state-of-the-art"` -> 4; `"Ünïcode"` -> 0 (starts with `ü`).
- Only HTML pages that are files (2xx) contribute words.

### 10.7 `PageReport::build` rules
| `FetchOutcome` | `outcome` | `extension` | `words` | `children` |
| --- | --- | --- | --- | --- |
| `Html { body }` | File | `extension(url)` | `count_words` | links from §10.5 |
| `File` | File | `extension(url)` | 0 | none |
| `Redirect { location }` | Redirect | — | 0 | `location` resolved against the URL, if valid and under base |
| `Broken` | Broken | — | 0 | none |

## 11. Correctness argument (README material)

**Claim once.** A URL enters a job queue only when `SADD seen` returns 1 for it, inside
`submit.lua` (start URL) or `finish.lua` (children). `SADD` returns 1 once per URL per job.

**Take once.** `take.lua` moves an item from a queue into exactly one processing list, atomically.

**Nothing lost.** A taken URL stays in a processing list in Redis until `finish.lua` removes it in
the same atomic step that records it as finished. If its node dies, the heartbeat key expires
within 10 s and a reaper moves the URL back to the queue. `pending` still counts it throughout.

**Count once, by the right reporter.** `finish.lua` only accepts a report from the loop that
currently holds the URL (its processing list still contains it), and only counts, claims
children and decrements if `SADD finished` returns 1. A zombie's late report is rejected,
because the reaper took the URL out of its processing list.

**Termination.** By invariant 1, `pending` = claimed - finished. In `finish.lua`, children are
added to `pending` before the parent is subtracted, inside one atomic script, so `pending` is
never 0 while any claimed URL is unfinished. Once 0, nothing can raise it: new claims only happen
inside reports of unfinished URLs, and there are none. `DECR` is atomic and each URL decrements
at most once, so exactly one script run observes 0 and marks the job done.

**Stats complete at done.** Each URL's stats are written in the same script as its decrement.

**Same answer for any N.** The set of claimed URLs is determined by the site, not by timing or N:
every linked URL under the base is claimed by someone exactly once. Stats are sums over that set,
and sums don't depend on order.

**Isolation.** All per-job state is under `job:{id}:`; scripts touch only one job's keys
(plus `jobs:active`). Shuffling changes speed and order only.

**At most 10 in flight per node.** 10 loops, each holding at most one URL.

### Crash points

| Crash moment | Effect | Recovery |
| --- | --- | --- |
| Before `take.lua` | Nothing taken | None needed |
| After `take.lua`, before `finish.lua` (e.g. mid-HTTP) | URL in `proc:{nid}:{loop}`; still counted in `pending` | Heartbeat expires; a reaper requeues it; another loop crawls it |
| During any script | Impossible: scripts are atomic | — |
| After `finish.lua` | Fully reported; processing list already emptied | None needed |
| Reaper killed mid-recovery | Each `requeue.lua` call is atomic per item | Another reaper (or a later run) continues |
| CLI killed during submit | `submit.lua` is atomic | Resubmit (returns the job if it was created) |
| Node frozen > 10 s, then resumes (zombie) | Its URL was requeued; it may be fetched twice, and its own requests may have timed out | Its processing list was emptied by the reaper, so `finish.lua` rejects its report (-2); only the new holder's result counts |
| Redis error during `finish` (node alive) | URL stays in the processing list | Next `take.lua` on that loop resumes it |

## 12. Worked example (tested trace)

Tested with `redis-cli --eval` against Redis 7.0 on the `test-site/` link structure. Nodes A
(loops 0-3) and B (loop 0). Node B is killed while holding `/a.html`, and later wakes as a zombie.
`B` = `http://localhost:8000/`.

| Step | Action | Script result |
| --- | --- | --- |
| 1 | submit `B` | `new 0001` |
| | submit `B` again | `existing 0001` |
| 2 | A0 take | `0001 B` |
| | A0 finish `/` (file, html, 29 words, children a.html, docs/, logo.svg, missing.html) | pending `4` |
| 3 | B0, A1, A2, A3 take | `a.html`, `docs/`, `logo.svg`, `missing.html` |
| 4 | A2 finish `logo.svg` (file, svg) | `3` |
| | A3 finish `missing.html` (broken) | `2` |
| 5 | A1 finish `docs/` (file, html, 7, child docs/intro.html) | `2` |
| 6 | **Node B killed.** `proc:B:0` = `0001 B a.html`, pending `2` | — |
| | Reaper: `requeue.lua proc:B:0` | `0001 B a.html`; queue = `docs/intro.html`, `a.html` |
| 7 | A0 take, A2 take | `docs/intro.html`, `a.html` |
| 8 | A0 finish `docs/intro.html` (file, html, 12, child index.html) | `2` |
| 9 | A3 take | `index.html` |
| 10 | A2 finish `a.html` (file, html, 11, children index.html, docs/intro.html: both already claimed) | `1` |
| 11 | **Zombie B0** reports `a.html` (as broken: its request timed out while frozen) | `-2` (rejected: B0 no longer holds it) |
| 12 | A3 finish `index.html` (file, html, 29, 4 children all already claimed) | `0` -> job done |

Final state: `meta.status = done`; `stats` = crawled 7, num_files 6, total_word_count 88,
broken 1; `ext` = html 5, svg 1; pending 0; seen 7; finished 7; `jobs:active` empty; no
processing lists left. These are exactly the stats of the same crawl without any crash.
(`/` and `/index.html` are different URLs, so the 29-word page is counted twice; documented choice.)

## 13. Fairness and scheduling

- Each take shuffles the active jobs' queue keys (`fastrand::shuffle`), then `take.lua` uses the
  first non-empty one. Among jobs with waiting URLs, each gets an equal share of takes on average.
- Why not rotation: with a fixed rotation, an empty job's turns all go to the same neighbor.
- Why not one global queue: first-come-first-served makes a small job wait behind a big job's
  backlog at every level of its crawl; per-job queues also give an exact `LLEN` frontier.
- Scheduling never affects results, only speed and order.
- Possible extensions (not required): weights or priorities per job; HTML-first ordering within a
  job using a sorted set, which reduces idle moments because only HTML pages reveal new links.

## 14. Configuration

| Setting | Default | Why |
| --- | --- | --- |
| Redis URL | `--redis` / `REDIS_URL`, default `redis://127.0.0.1:6379` | Nodes on other machines use the Docker host's IP |
| Worker loops per node | 10 | Spec maximum in flight |
| HTTP timeout | 10 s | A hanging server can't hold a loop forever |
| Fetch attempts / first retry pause | 3 / 250 ms, doubling | One hiccup must not turn a good page into a broken one (results would depend on luck) |
| Heartbeat refresh / TTL | 3 s / 10 s | Survives a couple of missed refreshes; detects death within ~10 s |
| Reaper interval | 5 s | Recovery within ~15 s of a crash |
| Empty-queue backoff | 50 ms doubling to 500 ms | Quick pickup when busy, little load when idle |
| `status -f` interval | 500 ms | |

## 15. Testing plan

**Unit tests (no Redis):** every `domain` function using the example tables in §10; `parse_html`
on small HTML strings (each tag/attribute in §10.5, script/style skipped, comments ignored);
`PageReport::build` for each row of §10.7; `WebStats` Display ordering.

**Script tests:** `scripts/test_scripts.sh` replays the §12 trace with `redis-cli --eval` and
checks the final state. Run it whenever a script changes.

**Integration (real Redis, use `FLUSHDB` or a separate DB index):** submit/resubmit; take/finish
round trip; status numbers during a crawl.

**Automated end-to-end suite:** `scripts/e2e_test.sh` runs everything below and prints PASS/FAIL.
Measured results: `test-site/` gives `files: 6   extensions: 2   words: 88` / `html 5   svg 1`
with 1 and 3 nodes; `test-site/extra/` gives `files: 8   extensions: 4   words: 98` /
`html 5   css 1   js 1   png 1` (plus 1 redirect, 1 broken); the generated 200-page site gives
`files: 221   extensions: 2   words: 2985` / `html 201   png 20` with no crash, with a node
killed (9 URLs recovered), and with a node frozen for 16 s (its late reports rejected).

**End to end with `test-site/`:** `python3 -m http.server 8000 --directory test-site`, then
`crawl submit http://localhost:8000/`. Expected with the current site:
`files: 6   extensions: 2   words: 88` / `html 5   svg 1` (crawled 7, broken 1).
Python's server redirects `/docs` to `/docs/`, so adding a link to `docs` exercises redirects.
Extend the site with a CSS file, an `<img src>`, a `<script src>` and a `?page=2` link, and update
the expected numbers.

**Same-answer test:** run with 1 node and with 3 nodes (`FLUSHALL` in between); outputs must match.
**Multi-job test:** submit two sites at once; both finish with their own correct stats.
**Crash test:** 3 nodes, a larger site, `kill -9` one node mid-crawl; the job finishes and stats
equal the no-crash run. **Zombie test:** `kill -STOP` a node for > 10 s, then `kill -CONT`; stats
still equal; logs show a `-1` duplicate.

## 16. README outline

1. What it is (one paragraph).
2. Quick start: prerequisites; start Redis (`docker compose up -d`, or the `docker run` line);
   build; `crawl ping`; launch N nodes (with `--redis redis://<ip>:6379`); submit, status,
   status -f, stats.
3. How nodes coordinate: per-job queues, pull model, `take.lua`, shuffling, claiming with `SADD`.
4. How progress is tracked: keys table; status numbers (frontier, in flight, crawled).
5. How the cluster knows a job is done: `pending` invariant and the argument from §11.
6. Crash recovery (extra challenge): processing lists, heartbeats, reapers, idempotent finish,
   crash-point table.
7. Corner-case decisions (§10) with reasons.
8. Testing: how to run unit tests, script tests, the same-answer and crash tests.

## 17. Known limitations and possible extensions

- Words split across inline tags (`Hel<b>lo</b>`) count as two words, since each text node is counted separately.
- Taking is polling-based (scripts can't block): up to ~500 ms pickup latency when idle.
- A node frozen longer than 10 s causes a duplicate fetch (counted once).
- Scripts don't roll back on a runtime error mid-way; keep them small and tested.
- Redis restarts are survived (timeouts on every command, plus reconnection driven by the heartbeat task; see the README). Redis losing its data is not: jobs disappear with it.
- robots.txt, politeness delays, JavaScript rendering and job cancellation are out of scope.
- `jobs:by_url` and per-job keys are never deleted (stats must persist); `FLUSHALL` clears everything.
- Extensions: weighted/priority jobs, HTML-first ordering, pub/sub for `status -f`,
  `connection-manager` for automatic Redis reconnects.
