//! Command-line definitions (clap). Only parsing lives here; behavior is in `commands/`.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "crawl", version, about = "A distributed web crawler coordinated through Redis")]
pub struct Cli {
    /// Redis address, e.g. redis://192.168.1.10:6379
    #[arg(long, global = true, env = "REDIS_URL", default_value = "redis://127.0.0.1:6379")]
    pub redis: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start a crawler node that joins the cluster (run this N times)
    Node {
        /// Don't print a line for every page
        #[arg(short, long)]
        quiet: bool,
    },
    /// Submit one or more URLs; each becomes its own crawl job
    Submit {
        #[arg(required = true)]
        urls: Vec<String>,
    },
    /// Show a job's progress, or every job's if no id is given
    Status {
        /// Job id, like 0001. Leave it out to list every job.
        job: Option<String>,
        /// Keep printing updates until the job is done
        #[arg(short, long)]
        follow: bool,
    },
    /// Print a job's WebStats (once the job is done)
    Stats { job: String },
    /// Check that Redis is reachable
    Ping,
}
