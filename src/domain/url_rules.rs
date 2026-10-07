//! URL rules: normalization, the base-path check, and extensions (docs/DESIGN.md §10).

use url::Url;

/// Parse an absolute URL and normalize it. Returns `None` for invalid or non-http(s) URLs.
pub fn normalize(raw: &str) -> Option<Url> {
    clean(Url::parse(raw.trim()).ok()?)
}

/// Resolve a link found on `page` (relative or absolute) and normalize it.
pub fn resolve(page: &Url, href: &str) -> Option<Url> {
    clean(page.join(href.trim()).ok()?)
}

/// The `url` crate already lowercases scheme and host, removes default ports,
/// resolves `.`/`..` and percent-encodes spaces. We only keep http(s) and drop fragments.
fn clean(mut url: Url) -> Option<Url> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

/// The spec: crawl only URLs that begin with the base path. A plain string prefix check
/// on normalized URLs (so a base without a trailing slash also matches longer names).
pub fn is_under_base(url: &str, base: &str) -> bool {
    url.starts_with(base)
}

/// Extension from the last path segment: text after the last `.`, lowercased.
/// No dot, or nothing after it, means `html`. Query strings never count.
pub fn extension(url: &Url) -> String {
    let segment = url.path().rsplit('/').next().unwrap_or("");
    match segment.rfind('.') {
        Some(i) if i + 1 < segment.len() => segment[i + 1..].to_lowercase(),
        _ => "html".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Url {
        Url::parse("http://h/docs/a.html").unwrap()
    }

    #[test]
    fn resolves_and_drops_fragments() {
        assert_eq!(resolve(&page(), "b.html#intro").unwrap().as_str(), "http://h/docs/b.html");
        assert_eq!(resolve(&page(), "../index.html").unwrap().as_str(), "http://h/index.html");
        assert_eq!(resolve(&page(), "/x?page=2").unwrap().as_str(), "http://h/x?page=2");
        assert_eq!(resolve(&page(), "  c.html  ").unwrap().as_str(), "http://h/docs/c.html");
        assert_eq!(resolve(&page(), "#top").unwrap().as_str(), "http://h/docs/a.html");
    }

    #[test]
    fn empty_fragment_is_dropped() {
        assert_eq!(resolve(&page(), "a.html#").unwrap().as_str(), "http://h/docs/a.html");
        assert_eq!(normalize("http://h/p.html#").unwrap().as_str(), "http://h/p.html");
    }

    #[test]
    fn query_strings_are_kept_and_fragments_dropped() {
        assert_eq!(resolve(&page(), "/search?q=rust#results").unwrap().as_str(), "http://h/search?q=rust");
        assert_ne!(resolve(&page(), "/search?q=rust"), resolve(&page(), "/search?q=postgres"));
    }

    #[test]
    fn percent_encoding_is_consistent() {
        // A raw space and %20 become the same URL.
        assert_eq!(resolve(&page(), "hello world.html"), resolve(&page(), "hello%20world.html"));
        assert_eq!(resolve(&page(), "hello%20world.html").unwrap().as_str(), "http://h/docs/hello%20world.html");
    }

    #[test]
    fn host_is_lowercased_but_path_case_is_kept() {
        assert_eq!(normalize("HTTP://EXAMPLE.COM/Docs/Page.html").unwrap().as_str(), "http://example.com/Docs/Page.html");
        assert_ne!(normalize("http://h/Page.html"), normalize("http://h/page.html"));
    }

    #[test]
    fn trailing_slash_matters() {
        assert_ne!(normalize("http://h/docs/intro"), normalize("http://h/docs/intro/"));
    }

    #[test]
    fn normalizes_case_and_ports() {
        assert_eq!(normalize("HTTP://H:80/Y").unwrap().as_str(), "http://h/Y");
        assert_eq!(normalize("http://localhost:8000").unwrap().as_str(), "http://localhost:8000/");
    }

    #[test]
    fn ignores_non_http() {
        assert!(resolve(&page(), "mailto:a@b.c").is_none());
        assert!(resolve(&page(), "javascript:void(0)").is_none());
        assert!(resolve(&page(), "tel:+6612345678").is_none());
        assert!(resolve(&page(), "data:image/png;base64,AAAA").is_none());
        assert!(normalize("ftp://h/file").is_none());
        assert!(normalize("not a url").is_none());
    }

    #[test]
    fn base_path_is_a_prefix_check() {
        assert!(is_under_base("http://h/docs/a.html", "http://h/docs/"));
        assert!(!is_under_base("http://h/other", "http://h/docs/"));
        assert!(is_under_base("http://h/docs-old/x", "http://h/docs"));
    }

    #[test]
    fn extensions() {
        let ext = |u: &str| extension(&Url::parse(u).unwrap());
        assert_eq!(ext("http://h/"), "html");
        assert_eq!(ext("http://h/docs/"), "html");
        assert_eq!(ext("http://h/a.HTML"), "html");
        assert_eq!(ext("http://h/img/Photo.JPG"), "jpg");
        assert_eq!(ext("http://h/img/photo.jpeg"), "jpeg");
        assert_eq!(ext("http://h/archive.tar.gz"), "gz");
        assert_eq!(ext("http://h/README"), "html");
        assert_eq!(ext("http://h/file."), "html");
        assert_eq!(ext("http://h/page.php?id=3"), "php");
    }
}
