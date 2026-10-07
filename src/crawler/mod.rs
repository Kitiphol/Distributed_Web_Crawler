//! What runs inside a `crawl node`: worker loops, the fetcher, the heartbeat and the reaper.

pub mod fetch;
pub mod heartbeat;
pub mod reaper;
pub mod worker;
