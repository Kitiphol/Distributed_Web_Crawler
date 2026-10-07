# Context for Claude Code

**What this file is for.** Claude Code reads this file automatically at the start of every
session. It is a short briefing: what the project is, where the real documentation is, and the
rules to follow. Details live in `docs/`.

University assignment: Mini 1, a distributed web crawler in Rust coordinated through Redis.
Spec: https://goms.cs.muzoo.io/projects/mini1.html (save a copy as `SPEC.md` in the repo root).

## Read first
- `docs/DESIGN.md`: the agreed design (WHAT to build). Source of truth. Ask before changing it.
- `docs/CONVERSATION_LOG.md`: how each decision was reached and what was rejected (WHY).

## About me
I'm learning async Rust and Redis. Explain what you change and why, keep code simple and
readable, and work in small steps I can review. I must be able to explain every part in my
README and live demo. When a choice isn't covered by DESIGN.md, ask me.

## Non-negotiables
- One binary `crawl`: `node`, `submit <url>...`, `status [-f] <job>`, `stats <job>`, `ping`.
- CLI and nodes talk only to Redis. Pull model, one queue per job, keys shuffled on every take.
- Multi-step updates are the Lua scripts in `scripts/` (`submit`, `take`, `finish`, `requeue`).
  They are tested: don't change them without updating `scripts/test_scripts.sh`
  and DESIGN.md §7 and §12. Only `src/store/` knows Redis commands, key names and scripts.
- Crash recovery: per-loop processing lists `proc:{node}:{loop}`, heartbeats
  `node:{nid}:alive` (SET EX 10, refreshed every 3 s), a reaper on every node, idempotent finish.
- Per node: `current_thread` Tokio runtime, 10 worker loops, at most 10 requests in flight.
- `scraper::Html` only inside synchronous parsing code; never held across `.await`.
- No blocking reqwest, no `std::thread::sleep`, no sync Redis API.

## Current state
Fully implemented per DESIGN.md. All tests pass: `cargo test` (18 unit tests),
`scripts/test_scripts.sh` (Lua scripts), `scripts/e2e_test.sh` (1 vs 3 nodes, two jobs,
kill -9, freeze/resume). `finish.lua` includes an ownership check added after testing
(DESIGN.md §7.3). README.md is written.

## Useful next steps
- Run `cargo clippy` and fix anything it reports (it wasn't available where the code was written).
- Help me understand the code module by module before my demo.
- Any change to the scripts must keep `scripts/test_scripts.sh` and `scripts/e2e_test.sh` passing.

## Commands
- Build and check: `cargo fmt && cargo clippy && cargo test`
- Redis: `docker compose up -d`
- End-to-end tests: `bash scripts/e2e_test.sh`
- Script tests: `bash scripts/test_scripts.sh` (needs local redis-cli) or
  `docker compose exec redis bash /scripts/test_scripts.sh`
- Test site: `python3 -m http.server 8000 --directory test-site`
- Commit in small steps with clear messages, on feature branches.
