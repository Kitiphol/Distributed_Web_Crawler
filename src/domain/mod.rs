//! Pure rules: no async, no Redis, no HTTP. Everything here is unit-tested.
//! These functions decide what counts as a file, its extension, its words and its links,
//! so they determine the final WebStats.

pub mod html;
pub mod job;
pub mod page;
pub mod stats;
pub mod url_rules;
pub mod words;
