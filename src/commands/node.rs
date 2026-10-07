//! `crawl node`: join the cluster and crawl until Ctrl+C.
//!
//! Runs 10 worker loops, a heartbeat and a reaper on one thread. On Ctrl+C it stops
//! taking new URLs, lets in-flight URLs finish, and leaves the cluster cleanly.
//! If it's killed instead, other nodes' reapers recover its URLs.

use crate::config::Config;
use crate::crawler::{fetch::HttpFetcher, heartbeat, reaper, worker::Worker};
use crate::store::{keys, RedisStore};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::task::JoinSet;

pub async fn run(cfg: Config, quiet: bool) -> Result<()> {
    let cfg = Arc::new(cfg);
    let store = RedisStore::connect(&cfg.redis_url).await?;
    let fetcher = HttpFetcher::new(cfg.http_timeout, cfg.fetch_attempts, cfg.retry_backoff, cfg.worker_loops)?;
    let node = format!("{:08x}", fastrand::u32(..));

    // Register before starting work, so reapers can always find our processing lists.
    store.heartbeat(&node, cfg.worker_loops, cfg.heartbeat_ttl_secs).await?;
    let heartbeat = tokio::spawn(heartbeat::run(store.clone(), cfg.clone(), node.clone()));
    let reaper = tokio::spawn(reaper::run(store.clone(), cfg.clone(), node.clone()));

    let (stop_tx, stop_rx) = watch::channel(false);
    let mut loops = JoinSet::new();
    for index in 0..cfg.worker_loops {
        let worker = Worker {
            store: store.clone(),
            fetcher: fetcher.clone(),
            cfg: cfg.clone(),
            node: node.clone(),
            index,
            quiet,
            shutdown: stop_rx.clone(),
        };
        loops.spawn(worker.run());
    }
    eprintln!("node {node} started with {} worker loops, Redis at {} (Ctrl+C to stop)", cfg.worker_loops, cfg.redis_url);

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("node {node}: stopping; finishing the URLs in progress...");
        }
        Some(result) = loops.join_next() => {
            // A worker loop only ends on a panic (a bug). Stop and report it.
            eprintln!("node {node}: a worker loop stopped unexpectedly: {result:?}");
        }
    }

    let _ = stop_tx.send(true);
    while loops.join_next().await.is_some() {}
    // Safety net: put back anything still held (normally every slot is empty by now).
    for index in 0..cfg.worker_loops {
        while let Some(item) = store.release(&keys::proc_list(&node, index)).await? {
            eprintln!("node {node}: put back {item}");
        }
    }
    heartbeat.abort();
    reaper.abort();
    store.deregister(&node).await?;
    eprintln!("node {node} left the cluster");
    Ok(())
}
