//! Thin glue: one module per subcommand.

pub mod node;
pub mod ping;
pub mod stats;
pub mod status;
pub mod submit;

use crate::domain::job::JobId;
use anyhow::{bail, Result};

/// Parse a job ID typed on the command line.
pub fn parse_job(raw: &str) -> Result<JobId> {
    match JobId::parse(raw) {
        Some(id) => Ok(id),
        None => bail!("'{raw}' is not a job id (job ids look like 0001)"),
    }
}
