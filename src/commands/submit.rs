//! `crawl submit <url>...`: one job per URL. Prints each job id and returns immediately.

use crate::config::Config;
use crate::domain::url_rules::normalize;
use crate::store::{RedisStore, SubmitResult};
use anyhow::{bail, Result};

pub async fn run(cfg: &Config, urls: &[String]) -> Result<()> {
    let store = RedisStore::connect(&cfg.redis_url).await?;
    let mut invalid = 0;
    for raw in urls {
        let Some(url) = normalize(raw) else {
            eprintln!("error: {raw}: not a valid http(s) URL");
            invalid += 1;
            continue;
        };
        match store.submit(url.as_str()).await? {
            SubmitResult::New(id) => println!("job {id}  {url}"),
            SubmitResult::Existing(id) => println!("job {id}  {url}  (already submitted)"),
        }
    }
    if invalid > 0 {
        bail!("{invalid} URL(s) were not submitted");
    }
    Ok(())
}
