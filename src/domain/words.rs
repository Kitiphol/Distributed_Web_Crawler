//! Word counting (docs/DESIGN.md §10.6).

/// Lowercase the text, split on every non-alphanumeric character, and count the
/// pieces that start with a letter `a`-`z`.
pub fn count_words(text: &str) -> u64 {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.starts_with(|c: char| c.is_ascii_lowercase()))
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples() {
        assert_eq!(count_words("Hello, World!"), 2);
        assert_eq!(count_words("An introduction with 123 numbers"), 4);
        assert_eq!(count_words("don't"), 2);
        assert_eq!(count_words("state-of-the-art"), 4);
        assert_eq!(count_words("Ünïcode"), 0);
        assert_eq!(count_words("   "), 0);
        assert_eq!(count_words("a1b2 9lives"), 1);
    }
}
