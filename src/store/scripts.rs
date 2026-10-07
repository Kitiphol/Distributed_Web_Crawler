//! The Lua scripts from scripts/, compiled into the binary (docs/DESIGN.md §7).
//! `redis::Script` sends a script by its hash (EVALSHA) and loads it automatically if needed.

use redis::Script;

pub struct Scripts {
    pub submit: Script,
    pub take: Script,
    pub finish: Script,
    pub requeue: Script,
}

impl Scripts {
    pub fn load() -> Self {
        Scripts {
            submit: Script::new(include_str!("../../scripts/submit.lua")),
            take: Script::new(include_str!("../../scripts/take.lua")),
            finish: Script::new(include_str!("../../scripts/finish.lua")),
            requeue: Script::new(include_str!("../../scripts/requeue.lua")),
        }
    }
}
