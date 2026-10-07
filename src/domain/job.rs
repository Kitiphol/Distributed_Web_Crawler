//! Job identifiers and job progress.

use std::fmt;

/// A job ID such as `0001`: the job counter in lowercase hex, at least 4 digits.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct JobId(String);

impl JobId {
    /// Build an ID from the counter value returned by `INCR jobs:next_id`.
    pub fn from_number(n: u64) -> Self {
        JobId(format!("{n:04x}"))
    }

    /// Parse an ID typed by a user or read from Redis. Accepts hex digits only.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(JobId(s))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A snapshot of a job's progress, as shown by `crawl status`.
#[derive(Debug, Clone, PartialEq)]
pub struct JobStatus {
    /// URLs finished (files, broken links and redirects).
    pub crawled: u64,
    /// URLs waiting to be taken.
    pub frontier: u64,
    /// URLs taken but not yet reported.
    pub in_flight: u64,
    pub files: u64,
    pub broken: u64,
    pub done: bool,
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "crawled {}   frontier {}   in flight {}   files {}   broken {}   {}",
            self.crawled,
            self.frontier,
            self.in_flight,
            self.files,
            self.broken,
            if self.done { "done" } else { "running" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_padded_lowercase_hex() {
        assert_eq!(JobId::from_number(1).as_str(), "0001");
        assert_eq!(JobId::from_number(255).as_str(), "00ff");
        assert_eq!(JobId::from_number(70000).as_str(), "11170");
    }

    #[test]
    fn parse_accepts_hex_only() {
        assert_eq!(JobId::parse(" 00FF ").unwrap().as_str(), "00ff");
        assert!(JobId::parse("").is_none());
        assert!(JobId::parse("job1").is_none());
    }

    #[test]
    fn status_line() {
        let s = JobStatus { crawled: 7, frontier: 0, in_flight: 0, files: 6, broken: 1, done: true };
        assert_eq!(s.to_string(), "crawled 7   frontier 0   in flight 0   files 6   broken 1   done");
    }
}
