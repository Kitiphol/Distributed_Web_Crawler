//! The result of a crawl, as defined by the assignment.

use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Default, PartialEq)]
pub struct WebStats {
    /// the total number of (unique) files found
    pub num_files: usize,
    /// the total number of (unique) file extensions (.jpg is different from .jpeg)
    pub num_exts: usize,
    /// the total number of files for each extension
    pub ext_counts: HashMap<String, usize>,
    /// the total number of words in all HTML files combined, excluding
    /// all HTML tags, attributes, and HTML comments
    pub total_word_count: u64,
}

impl fmt::Display for WebStats {
    /// files: 6   extensions: 2   words: 88
    ///   html 5   svg 1
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "files: {}   extensions: {}   words: {}",
            self.num_files, self.num_exts, self.total_word_count
        )?;
        // Most common first; ties by name, so output is stable and easy to compare.
        let mut exts: Vec<_> = self.ext_counts.iter().collect();
        exts.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        if !exts.is_empty() {
            let line: Vec<String> = exts.iter().map(|(e, n)| format!("{e} {n}")).collect();
            write!(f, "\n  {}", line.join("   "))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_sorts_by_count_then_name() {
        let stats = WebStats {
            num_files: 4,
            num_exts: 3,
            ext_counts: HashMap::from([("png".into(), 1), ("html".into(), 2), ("css".into(), 1)]),
            total_word_count: 42,
        };
        assert_eq!(stats.to_string(), "files: 4   extensions: 3   words: 42\n  html 2   css 1   png 1");
    }

    #[test]
    fn display_without_files() {
        assert_eq!(WebStats::default().to_string(), "files: 0   extensions: 0   words: 0");
    }
}
