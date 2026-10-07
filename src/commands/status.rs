//! `crawl status [-f] [job]`: one job's progress, or a table of every job.
//! With -f, keep refreshing until the job (or every job) is done.

use super::parse_job;
use crate::config::Config;
use crate::domain::job::JobId;
use crate::store::RedisStore;
use anyhow::{bail, Result};

pub async fn run(cfg: &Config, job: Option<&str>, follow: bool) -> Result<()> {
    let store = RedisStore::connect(&cfg.redis_url).await?;
    match job {
        Some(raw) => one_job(&store, cfg, &parse_job(raw)?, follow).await,
        None => all_jobs(&store, cfg, follow).await,
    }
}

/// `crawl status 0001`: one line per refresh.
async fn one_job(store: &RedisStore, cfg: &Config, job: &JobId, follow: bool) -> Result<()> {
    loop {
        let Some(status) = store.status(job).await? else {
            bail!("no such job {job}");
        };
        println!("job {job}  {status}");
        if !follow || status.done {
            return Ok(());
        }
        tokio::time::sleep(cfg.follow_every).await;
    }
}

/// `crawl status`: a table with one row per job, oldest first.
async fn all_jobs(store: &RedisStore, cfg: &Config, follow: bool) -> Result<()> {
    loop {
        let jobs = store.all_jobs().await?;
        if jobs.is_empty() {
            println!("no jobs yet; submit one with: crawl submit <url>");
            return Ok(());
        }
        println!(
            "{:<6} {:<8} {:>8} {:>9} {:>10} {:>7} {:>7}  {}",
            "job", "status", "crawled", "frontier", "in flight", "files", "broken", "url"
        );
        let mut all_done = true;
        for (id, url) in &jobs {
            if let Some(s) = store.status(id).await? {
                all_done = all_done && s.done;
                println!(
                    "{:<6} {:<8} {:>8} {:>9} {:>10} {:>7} {:>7}  {}",
                    id.as_str(),
                    if s.done { "done" } else { "running" },
                    s.crawled,
                    s.frontier,
                    s.in_flight,
                    s.files,
                    s.broken,
                    url
                );
            }
        }
        if !follow || all_done {
            return Ok(());
        }
        println!();
        tokio::time::sleep(cfg.follow_every).await;
    }
}
