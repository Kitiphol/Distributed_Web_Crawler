//! Every Redis key name in one place (docs/DESIGN.md §6).

use crate::domain::job::JobId;

/// INT: job ID counter
pub const NEXT_JOB_ID: &str = "jobs:next_id";
/// HASH url -> job id: submit-time dedup
pub const JOBS_BY_URL: &str = "jobs:by_url";
/// SET of job ids: jobs that workers should take from
pub const JOBS_ACTIVE: &str = "jobs:active";
/// HASH node id -> worker loop count: nodes the reapers watch
pub const NODES: &str = "nodes";

/// STRING with expiry: exists while the node is alive
pub fn node_alive(node: &str) -> String {
    format!("node:{node}:alive")
}
/// LIST with 0 or 1 items ("<job> <url>"): what one worker loop is holding
pub fn proc_list(node: &str, loop_index: usize) -> String {
    format!("proc:{node}:{loop_index}")
}

fn job(id: &JobId, part: &str) -> String {
    format!("job:{id}:{part}")
}
/// HASH: base, status, submitted_at, finished_at
pub fn meta(id: &JobId) -> String {
    job(id, "meta")
}
/// LIST: the frontier
pub fn queue(id: &JobId) -> String {
    job(id, "queue")
}
/// SET: every URL ever claimed
pub fn seen(id: &JobId) -> String {
    job(id, "seen")
}
/// SET: every URL already reported
pub fn finished(id: &JobId) -> String {
    job(id, "finished")
}
/// INT: claimed but not finished
pub fn pending(id: &JobId) -> String {
    job(id, "pending")
}
/// HASH: crawled, num_files, broken, redirects, total_word_count
pub fn stats(id: &JobId) -> String {
    job(id, "stats")
}
/// HASH extension -> count
pub fn ext(id: &JobId) -> String {
    job(id, "ext")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_the_lua_scripts() {
        let id = JobId::from_number(1);
        assert_eq!(queue(&id), "job:0001:queue");
        assert_eq!(finished(&id), "job:0001:finished");
        assert_eq!(proc_list("3fa9c210", 4), "proc:3fa9c210:4");
        assert_eq!(node_alive("3fa9c210"), "node:3fa9c210:alive");
    }
}
