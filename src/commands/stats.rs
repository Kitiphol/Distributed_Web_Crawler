//! `crawl stats <job>`: the job's WebStats, once it's done.

use super::parse_job;
use crate::config::Config;
use crate::store::{RedisStore, StatsResult};
use anyhow::{bail, Result};

pub async fn run(cfg: &Config, job: &str) -> Result<()> {
    let job = parse_job(job)?;
    let store = RedisStore::connect(&cfg.redis_url).await?;
    match store.stats(&job).await? {
        StatsResult::NoSuchJob => bail!("no such job {job}"),
        StatsResult::Running { crawled } => {
            println!("job {job} is still running ({crawled} URLs crawled so far); try again when it's done")
        }
        StatsResult::Done(stats) => println!("{stats}"),
    }
    Ok(())
}
