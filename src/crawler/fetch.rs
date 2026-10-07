//! HTTP: HEAD every URL, GET only HTML (docs/DESIGN.md §9.6).
//!
//! Temporary failures (network errors, timeouts, 5xx, 429) are retried a few times with a
//! growing pause, so one hiccup doesn't turn a good page into a "broken" one. Answers that
//! won't change on retry (2xx, 3xx, other 4xx) are final immediately.

use crate::domain::page::FetchOutcome;
use anyhow::Result;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use reqwest::{redirect, Client, Response, StatusCode};
use std::time::Duration;
use tokio::time::sleep;

/// Cheap to clone: clones share one connection pool.
#[derive(Clone)]
pub struct HttpFetcher {
    client: Client,
    attempts: u32,
    backoff: Duration,
}

/// What the headers of a response tell us.
enum Kind {
    Html,
    File,
    Redirect(Option<String>),
    /// A final failure, like 404: retrying won't help.
    Broken,
    /// A failure that may go away: network error, timeout, 5xx, 429.
    Temporary,
}

/// The result of one attempt.
enum Attempt {
    Done(FetchOutcome),
    Temporary,
}

fn classify(resp: &Response) -> Kind {
    let status = resp.status();
    if status.is_success() {
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.starts_with("text/html") || content_type.starts_with("application/xhtml+xml") {
            Kind::Html
        } else {
            Kind::File
        }
    } else if status.is_redirection() {
        let location = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()).map(String::from);
        Kind::Redirect(location)
    } else if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        Kind::Temporary
    } else {
        Kind::Broken
    }
}

impl HttpFetcher {
    /// `idle_per_host`: how many open connections to keep per website for reuse
    /// (one per worker loop is enough, since each loop makes one request at a time).
    pub fn new(timeout: Duration, attempts: u32, backoff: Duration, idle_per_host: usize) -> Result<Self> {
        let client = Client::builder()
            .timeout(timeout)
            // Keep-alive: connections are pooled and reused across requests and loops, so
            // only the first request to a site pays for the TCP (and TLS) handshake.
            .pool_max_idle_per_host(idle_per_host)
            .pool_idle_timeout(Duration::from_secs(90))
            // TCP keepalive probes notice connections that died silently.
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_nodelay(true)
            .redirect(redirect::Policy::none()) // a redirect target is treated as a new link
            .user_agent(concat!("crawl-mini1/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(HttpFetcher { client, attempts: attempts.max(1), backoff })
    }

    /// Fetch a URL, retrying temporary failures. Gives `Broken` if every attempt fails.
    pub async fn fetch(&self, url: &str) -> FetchOutcome {
        let mut pause = self.backoff;
        for attempt in 1..=self.attempts {
            match self.fetch_once(url).await {
                Attempt::Done(outcome) => return outcome,
                Attempt::Temporary if attempt < self.attempts => {
                    sleep(pause).await;
                    pause *= 2;
                }
                Attempt::Temporary => {}
            }
        }
        FetchOutcome::Broken
    }

    async fn fetch_once(&self, url: &str) -> Attempt {
        let head = match self.client.head(url).send().await {
            Ok(resp) => resp,
            Err(_) => return Attempt::Temporary, // connection refused/reset, timeout, ...
        };
        // Some servers don't support HEAD: fall back to GET.
        if head.status() == StatusCode::METHOD_NOT_ALLOWED || head.status() == StatusCode::NOT_IMPLEMENTED {
            return self.get(url).await;
        }
        match classify(&head) {
            Kind::Html => self.get(url).await,
            Kind::File => Attempt::Done(FetchOutcome::File),
            Kind::Redirect(location) => Attempt::Done(FetchOutcome::Redirect { location }),
            Kind::Broken => Attempt::Done(FetchOutcome::Broken),
            Kind::Temporary => Attempt::Temporary,
        }
    }

    async fn get(&self, url: &str) -> Attempt {
        let resp = match self.client.get(url).send().await {
            Ok(resp) => resp,
            Err(_) => return Attempt::Temporary,
        };
        match classify(&resp) {
            Kind::Html => match resp.text().await {
                Ok(body) => Attempt::Done(FetchOutcome::Html { body }),
                Err(_) => Attempt::Temporary, // the connection broke while reading the body
            },
            // Dropping `resp` without reading the body skips downloading it.
            Kind::File => Attempt::Done(FetchOutcome::File),
            Kind::Redirect(location) => Attempt::Done(FetchOutcome::Redirect { location }),
            Kind::Broken => Attempt::Done(FetchOutcome::Broken),
            Kind::Temporary => Attempt::Temporary,
        }
    }
}
