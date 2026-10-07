//! `crawl ping`: check that Redis is reachable.

use crate::config::Config;
use crate::store::RedisStore;
use anyhow::Result;

pub async fn run(cfg: &Config) -> Result<()> {
    let store = RedisStore::connect(&cfg.redis_url).await?;
    println!("redis replied {}", store.ping().await?);
    Ok(())
}
