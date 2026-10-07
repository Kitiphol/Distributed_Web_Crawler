//! One worker loop: take a URL, fetch it, build its report, report it. Repeat.
//! Each loop holds at most one URL, so 10 loops = at most 10 requests in flight.

use crate::config::Config;
use crate::crawler::fetch::HttpFetcher;
use crate::domain::job::JobId;
use crate::domain::page::{FetchOutcome, PageReport};
use crate::store::{keys, RedisStore};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{sleep, Instant};
use url::Url;

pub struct Worker {
    pub store: RedisStore,
    pub fetcher: HttpFetcher,
    pub cfg: Arc<Config>,
    pub node: String,
    pub index: usize,
    pub quiet: bool,
    /// Becomes `true` when the node is shutting down (Ctrl+C).
    pub shutdown: watch::Receiver<bool>,
}

impl Worker {
    fn log(&self, msg: &str) {
        eprintln!("[{} loop {}] {msg}", self.node, self.index);
    }

    pub async fn run(self) {
        let proc_key = keys::proc_list(&self.node, self.index);
        let mut backoff = self.cfg.backoff_min;
        // The running jobs, re-read from Redis at most once per `jobs_refresh`
        // (saves a round trip per URL). `None` forces a fresh read.
        let mut jobs: Vec<JobId> = Vec::new();
        let mut jobs_read_at: Option<Instant> = None;

        loop {
            if *self.shutdown.borrow() {
                break;
            }

            // 1. Which jobs have work? Shuffle them so every job gets a fair share.
            if jobs_read_at.map_or(true, |t| t.elapsed() >= self.cfg.jobs_refresh) {
                match self.store.active_jobs().await {
                    Ok(j) => {
                        jobs = j;
                        jobs_read_at = Some(Instant::now());
                    }
                    Err(e) => {
                        self.log(&format!("Redis error: {e:#}"));
                        sleep(self.cfg.error_pause).await;
                        continue;
                    }
                }
            }
            fastrand::shuffle(&mut jobs);

            // 2. Take one URL (moved into this loop's processing list in Redis).
            let taken = if jobs.is_empty() {
                Ok(None)
            } else {
                self.store.take(&proc_key, &jobs).await
            };
            let (job, url) = match taken {
                Ok(Some(t)) => t,
                Ok(None) => {
                    // Nothing to do: wait a little longer each time, up to the max,
                    // and re-read the job list next time (a new job may have arrived).
                    jobs_read_at = None;
                    self.idle(backoff).await;
                    backoff = (backoff * 2).min(self.cfg.backoff_max);
                    continue;
                }
                Err(e) => {
                    self.log(&format!("Redis error: {e:#}"));
                    sleep(self.cfg.error_pause).await;
                    continue;
                }
            };
            backoff = self.cfg.backoff_min;

            // 3. The job's base path. Asked every time: after Redis is wiped, the same
            //    job ID can belong to a different site, so the answer must not be cached.
            let base = match self.store.job_base(&job).await {
                Ok(Some(b)) => b,
                Ok(None) => {
                    self.log(&format!("job {job} no longer exists; dropping {url}"));
                    let _ = self.store.release(&proc_key).await;
                    continue;
                }
                Err(e) => {
                    self.log(&format!("Redis error: {e:#}"));
                    sleep(self.cfg.error_pause).await;
                    continue; // the URL stays in our slot; the next take resumes it
                }
            };

            // 4. Fetch, then build the report with pure rules (parsing happens here,
            //    synchronously, so the parsed HTML never lives across an .await).
            let report = match Url::parse(&url) {
                Ok(page_url) => {
                    let fetched = self.fetcher.fetch(&url).await;
                    PageReport::build(&page_url, &base, fetched)
                }
                Err(_) => PageReport::build(
                    &Url::parse(&base).expect("base is a valid URL"),
                    &base,
                    FetchOutcome::Broken,
                ),
            };

            // 5. Report it, atomically. Retry until Redis accepts it: reporting is
            //    idempotent, so a retry can never count the page twice.
            loop {
                match self.store.finish(&proc_key, &job, &url, &report).await {
                    Ok(left) => {
                        if !self.quiet {
                            let what = match report.outcome {
                                crate::domain::page::Outcome::File => {
                                    format!("file {}", report.extension)
                                }
                                other => other.as_str().to_string(),
                            };
                            let extra = match left {
                                -2 => "  (no longer ours: recovered by another node, rejected)",
                                -1 => "  (already reported, ignored)",
                                _ => "",
                            };
                            self.log(&format!(
                                "job {job}  {what:<10} {url}  +{} links{extra}",
                                report.children.len()
                            ));
                        }
                        if left == 0 {
                            self.log(&format!("job {job} finished"));
                        }
                        break;
                    }
                    Err(e) => {
                        self.log(&format!("Redis error while reporting {url}: {e:#}; retrying"));
                        sleep(self.cfg.error_pause).await;
                    }
                }
            }
        }
    }

    /// Sleep, but wake up early if the node starts shutting down.
    async fn idle(&self, d: std::time::Duration) {
        let mut shutdown = self.shutdown.clone();
        tokio::select! {
            _ = sleep(d) => {}
            _ = shutdown.changed() => {}
        }
    }
}