//! All Redis operations used by the CLI and the nodes.

use super::keys;
use super::scripts::Scripts;
use crate::domain::job::{JobId, JobStatus};
use crate::domain::page::PageReport;
use crate::domain::stats::WebStats;
use anyhow::{Context, Result};
use redis::aio::MultiplexedConnection;
use redis::AsyncConnectionConfig;
use redis::AsyncCommands;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A cheap-to-clone handle. All clones share ONE long-lived connection, kept open and reused
/// for every command (no new TCP connection per command), so every worker loop can have its
/// own handle. If Redis restarts, `reconnect` swaps in a fresh connection for all clones at once.
#[derive(Clone)]
pub struct RedisStore {
    client: redis::Client,
    con: Arc<Mutex<MultiplexedConnection>>,
    scripts: Arc<Scripts>,
}

/// Timeouts so a command can never hang forever (for example if Redis dies mid-command):
/// it fails instead, the caller retries, and the heartbeat task reconnects.
fn connection_config() -> AsyncConnectionConfig {
    AsyncConnectionConfig::new()
        .set_connection_timeout(Duration::from_secs(2))
        .set_response_timeout(Duration::from_secs(5))
}

pub enum SubmitResult {
    New(JobId),
    Existing(JobId),
}

pub enum StatsResult {
    NoSuchJob,
    Running { crawled: u64 },
    Done(WebStats),
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl RedisStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).with_context(|| format!("invalid Redis URL {url}"))?;
        let con = client
            .get_multiplexed_async_connection_with_config(&connection_config())
            .await
            .with_context(|| format!("can't connect to Redis at {url} (is it running?)"))?;
        Ok(RedisStore { client, con: Arc::new(Mutex::new(con)), scripts: Arc::new(Scripts::load()) })
    }

    /// The shared connection (a cheap clone). The lock is held only for this line,
    /// never across an `.await`.
    fn con(&self) -> MultiplexedConnection {
        self.con.lock().expect("connection lock").clone()
    }

    /// Open a fresh connection and make every clone of this store use it.
    /// Called by the heartbeat task when Redis stops answering (e.g. after a restart).
    pub async fn reconnect(&self) -> Result<()> {
        let fresh = self.client.get_multiplexed_async_connection_with_config(&connection_config()).await?;
        *self.con.lock().expect("connection lock") = fresh;
        Ok(())
    }

    pub async fn ping(&self) -> Result<String> {
        Ok(redis::cmd("PING").query_async(&mut self.con()).await?)
    }

    /// Create a job for an (already normalized) URL, or return the existing one.
    pub async fn submit(&self, url: &str) -> Result<SubmitResult> {
        let mut con = self.con();
        let n: u64 = con.incr(keys::NEXT_JOB_ID, 1).await?;
        let candidate = JobId::from_number(n);
        let (kind, id): (String, String) = self
            .scripts
            .submit
            .key(keys::JOBS_BY_URL)
            .key(keys::JOBS_ACTIVE)
            .arg(url)
            .arg(candidate.as_str())
            .arg(now())
            .invoke_async(&mut con)
            .await?;
        let id = JobId::parse(&id).context("Redis returned an invalid job id")?;
        Ok(if kind == "new" { SubmitResult::New(id) } else { SubmitResult::Existing(id) })
    }

    /// Every job ever submitted (running or done), as (id, url), oldest first.
    pub async fn all_jobs(&self) -> Result<Vec<(JobId, String)>> {
        let by_url: HashMap<String, String> = self.con().hgetall(keys::JOBS_BY_URL).await?;
        let mut jobs: Vec<(JobId, String)> = by_url
            .into_iter()
            .filter_map(|(url, id)| JobId::parse(&id).map(|id| (id, url)))
            .collect();
        // Ids are hex padded to at least 4 digits, so (length, text) gives numeric order.
        jobs.sort_by(|a, b| (a.0.as_str().len(), a.0.as_str()).cmp(&(b.0.as_str().len(), b.0.as_str())));
        Ok(jobs)
    }

    pub async fn active_jobs(&self) -> Result<Vec<JobId>> {
        let ids: Vec<String> = self.con().smembers(keys::JOBS_ACTIVE).await?;
        Ok(ids.iter().filter_map(|s| JobId::parse(s)).collect())
    }

    pub async fn job_base(&self, job: &JobId) -> Result<Option<String>> {
        Ok(self.con().hget(keys::meta(job), "base").await?)
    }

    /// Move one URL from the first non-empty queue (in the given order) into this loop's
    /// processing list. If the loop still holds a URL, that one is returned again.
    pub async fn take(&self, proc_key: &str, jobs: &[JobId]) -> Result<Option<(JobId, String)>> {
        let mut inv = self.scripts.take.prepare_invoke();
        inv.key(proc_key);
        for job in jobs {
            inv.key(keys::queue(job));
        }
        for job in jobs {
            inv.arg(job.as_str());
        }
        let taken: Option<(String, String)> = inv.invoke_async(&mut self.con()).await?;
        Ok(taken.and_then(|(id, url)| JobId::parse(&id).map(|id| (id, url))))
    }

    /// Report a finished URL. Returns pending after the report (0 = job finished),
    /// -2 if this loop no longer holds the URL (a reaper recovered it), or
    /// -1 if this URL had already been reported.
    pub async fn finish(&self, proc_key: &str, job: &JobId, url: &str, report: &PageReport) -> Result<i64> {
        let mut inv = self.scripts.finish.prepare_invoke();
        inv.key(proc_key)
            .key(keys::seen(job))
            .key(keys::queue(job))
            .key(keys::pending(job))
            .key(keys::stats(job))
            .key(keys::ext(job))
            .key(keys::finished(job))
            .key(keys::meta(job))
            .key(keys::JOBS_ACTIVE)
            .arg(job.as_str())
            .arg(url)
            .arg(report.outcome.as_str())
            .arg(&report.extension)
            .arg(report.words)
            .arg(now());
        for child in &report.children {
            inv.arg(child);
        }
        Ok(inv.invoke_async(&mut self.con()).await?)
    }

    /// Announce that a node is alive (also re-registers it after a long freeze).
    pub async fn heartbeat(&self, node: &str, loops: usize, ttl_secs: u64) -> Result<()> {
        let _: () = redis::pipe()
            .hset(keys::NODES, node, loops)
            .ignore()
            .set_ex(keys::node_alive(node), 1, ttl_secs)
            .ignore()
            .query_async(&mut self.con())
            .await?;
        Ok(())
    }

    /// Remove a node from the registry (graceful shutdown).
    pub async fn deregister(&self, node: &str) -> Result<()> {
        let _: () = redis::pipe()
            .hdel(keys::NODES, node)
            .ignore()
            .del(keys::node_alive(node))
            .ignore()
            .query_async(&mut self.con())
            .await?;
        Ok(())
    }

    /// Registered nodes (other than `me`) whose heartbeat has expired, with their loop counts.
    pub async fn dead_nodes(&self, me: &str) -> Result<Vec<(String, usize)>> {
        let mut con = self.con();
        let nodes: HashMap<String, usize> = con.hgetall(keys::NODES).await?;
        let mut dead = Vec::new();
        for (node, loops) in nodes {
            if node == me {
                continue;
            }
            let alive: bool = con.exists(keys::node_alive(&node)).await?;
            if !alive {
                dead.push((node, loops));
            }
        }
        Ok(dead)
    }

    /// Move one item from a dead loop's processing list back to its job queue.
    /// Returns the moved item, or None when the list is empty.
    pub async fn requeue(&self, proc_key: &str) -> Result<Option<String>> {
        Ok(self.scripts.requeue.key(proc_key).invoke_async(&mut self.con()).await?)
    }

    /// Forget a dead node once its processing lists are empty.
    pub async fn forget_node(&self, node: &str) -> Result<()> {
        let _: () = self.con().hdel(keys::NODES, node).await?;
        Ok(())
    }

    /// Put this loop's held URL back in its queue (used on graceful shutdown).
    pub async fn release(&self, proc_key: &str) -> Result<Option<String>> {
        self.requeue(proc_key).await
    }

    /// One consistent snapshot of a job's progress. None if the job doesn't exist.
    pub async fn status(&self, job: &JobId) -> Result<Option<JobStatus>> {
        let (status, frontier, pending, stats): (Option<String>, u64, Option<i64>, HashMap<String, u64>) =
            redis::pipe()
                .atomic()
                .hget(keys::meta(job), "status")
                .llen(keys::queue(job))
                .get(keys::pending(job))
                .hgetall(keys::stats(job))
                .query_async(&mut self.con())
                .await?;
        let Some(status) = status else { return Ok(None) };
        let pending = pending.unwrap_or(0).max(0) as u64;
        let get = |k: &str| stats.get(k).copied().unwrap_or(0);
        Ok(Some(JobStatus {
            crawled: get("crawled"),
            frontier,
            in_flight: pending.saturating_sub(frontier),
            files: get("num_files"),
            broken: get("broken"),
            done: status == "done",
        }))
    }

    pub async fn stats(&self, job: &JobId) -> Result<StatsResult> {
        let (status, stats, ext): (Option<String>, HashMap<String, u64>, HashMap<String, usize>) = redis::pipe()
            .atomic()
            .hget(keys::meta(job), "status")
            .hgetall(keys::stats(job))
            .hgetall(keys::ext(job))
            .query_async(&mut self.con())
            .await?;
        let get = |k: &str| stats.get(k).copied().unwrap_or(0);
        Ok(match status.as_deref() {
            None => StatsResult::NoSuchJob,
            Some("done") => StatsResult::Done(WebStats {
                num_files: get("num_files") as usize,
                num_exts: ext.len(),
                ext_counts: ext,
                total_word_count: get("total_word_count"),
            }),
            Some(_) => StatsResult::Running { crawled: get("crawled") },
        })
    }
}
