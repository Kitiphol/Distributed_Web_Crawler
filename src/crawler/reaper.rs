//! Recovers the work of dead nodes (docs/DESIGN.md §9.5). Every node runs one,
//! so any surviving node can recover any dead node's URLs.

use crate::config::Config;
use crate::store::{keys, RedisStore};
use std::sync::Arc;
use tokio::time::{interval, MissedTickBehavior};

pub async fn run(store: RedisStore, cfg: Arc<Config>, me: String) {
    let mut tick = interval(cfg.reaper_every);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = reap_once(&store, &me).await {
            eprintln!("[{me} reaper] Redis error: {e:#}");
        }
    }
}

async fn reap_once(store: &RedisStore, me: &str) -> anyhow::Result<()> {
    for (node, loops) in store.dead_nodes(me).await? {
        let mut moved = 0;
        for index in 0..loops {
            let proc_key = keys::proc_list(&node, index);
            while let Some(item) = store.requeue(&proc_key).await? {
                eprintln!("[{me} reaper] node {node} is dead; put back: {item}");
                moved += 1;
            }
        }
        store.forget_node(&node).await?;
        eprintln!("[{me} reaper] forgot dead node {node} ({moved} URL(s) recovered)");
    }
    Ok(())
}
