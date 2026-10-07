//! All tunable settings in one place (docs/DESIGN.md section 14).

use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    /// Where Redis is, e.g. redis://192.168.1.10:6379
    pub redis_url: String,
    /// Worker loops per node. Each holds at most one URL, so this is also the
    /// maximum number of requests in flight per node (the spec allows 10).
    pub worker_loops: usize,
    /// Give up on a request after this long.
    pub http_timeout: Duration,
    /// Attempts per URL for temporary failures (network errors, timeouts, 5xx, 429).
    pub fetch_attempts: u32,
    /// Wait before the first retry; doubles for each further retry.
    pub retry_backoff: Duration,
    /// How often a node renews its heartbeat.
    pub heartbeat_every: Duration,
    /// How long a heartbeat lives without renewal (seconds).
    pub heartbeat_ttl_secs: u64,
    /// How often each node's reaper looks for dead nodes.
    pub reaper_every: Duration,
    /// How long a worker loop reuses its list of running jobs before reading it again.
    /// Saves a Redis round trip per URL; a new job is noticed within this time.
    pub jobs_refresh: Duration,
    /// Waiting when there's no work: start here, double each time, stop at max.
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Pause after a Redis error before retrying.
    pub error_pause: Duration,
    /// How often `crawl status -f` refreshes.
    pub follow_every: Duration,
}

impl Config {
    pub fn new(redis_url: String) -> Self {
        Config {
            redis_url,
            worker_loops: 10,
            http_timeout: Duration::from_secs(10),
            fetch_attempts: 3,
            retry_backoff: Duration::from_millis(250),
            heartbeat_every: Duration::from_secs(3),
            heartbeat_ttl_secs: 10,
            reaper_every: Duration::from_secs(5),
            jobs_refresh: Duration::from_secs(1),
            backoff_min: Duration::from_millis(50),
            backoff_max: Duration::from_millis(500),
            error_pause: Duration::from_secs(1),
            follow_every: Duration::from_millis(500),
        }
    }
}
