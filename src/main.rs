//! `crawl`: a distributed web crawler coordinated through Redis (Mini 1).
//!
//! One binary provides both the crawler nodes (`crawl node`) and the command-line
//! tool (`crawl submit`, `status`, `stats`). Everything shared lives in Redis.
//! See docs/DESIGN.md for the full design.

mod cli;
mod commands;
mod config;
mod crawler;
mod domain;
mod store;

use clap::Parser;
use cli::{Cli, Command};
use config::Config;

// One thread per process. Concurrency comes from Tokio switching between tasks
// whenever one of them waits on the network.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let cfg = Config::new(cli.redis);

    let result = match cli.command {
        Command::Node { quiet } => commands::node::run(cfg, quiet).await,
        Command::Submit { urls } => commands::submit::run(&cfg, &urls).await,
        Command::Status { job, follow } => commands::status::run(&cfg, job.as_deref(), follow).await,
        Command::Stats { job } => commands::stats::run(&cfg, &job).await,
        Command::Ping => commands::ping::run(&cfg).await,
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
