//! Renews this node's heartbeat so reapers know it's alive (docs/DESIGN.md §9.4).
//! A separate task, so a slow HTTP request never delays it.
//!
//! It also watches the Redis connection: if a heartbeat fails (for example because Redis
//! restarted), it opens a fresh connection, which every worker loop of this node then uses.

use crate::config::Config;
use crate::store::RedisStore;
use std::sync::Arc;
use tokio::time::{interval, MissedTickBehavior};

pub async fn run(store: RedisStore, cfg: Arc<Config>, node: String) {
    let mut tick = interval(cfg.heartbeat_every);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut connected = true;
    loop {
        tick.tick().await;
        match store.heartbeat(&node, cfg.worker_loops, cfg.heartbeat_ttl_secs).await {
            Ok(()) => {
                if !connected {
                    eprintln!("[{node} heartbeat] Redis is back");
                    connected = true;
                }
            }
            Err(e) => {
                if connected {
                    eprintln!("[{node} heartbeat] lost Redis ({e:#}); reconnecting every {:?}", cfg.heartbeat_every);
                    connected = false;
                }
                // Try a fresh connection; the next tick checks whether it works.
                let _ = store.reconnect().await;
            }
        }
    }
}
