//! Turning a fetch result into a report (docs/DESIGN.md §10.7). Pure: the same input
//! always gives the same report, on any node.

use super::html::parse_html;
use super::url_rules::{extension, is_under_base, resolve};
use url::Url;

/// What the fetcher found at a URL.
#[derive(Debug)]
pub enum FetchOutcome {
    /// 2xx with an HTML Content-Type: the downloaded body.
    Html { body: String },
    /// 2xx, not HTML. Exists, so it's a file; not downloaded.
    File,
    /// 3xx with its Location header (if any).
    Redirect { location: Option<String> },
    /// 4xx, 5xx, timeout or network error.
    Broken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    File,
    Broken,
    Redirect,
}

impl Outcome {
    /// The name the `finish.lua` script expects.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::File => "file",
            Outcome::Broken => "broken",
            Outcome::Redirect => "redirect",
        }
    }
}

/// Everything Redis needs to know about one finished URL.
#[derive(Debug, Clone, PartialEq)]
pub struct PageReport {
    pub outcome: Outcome,
    /// Only meaningful when `outcome` is `File`.
    pub extension: String,
    /// Non-zero only for HTML files.
    pub words: u64,
    /// New candidate URLs: normalized, under the base path, without duplicates,
    /// never the page itself.
    pub children: Vec<String>,
}

impl PageReport {
    pub fn build(page_url: &Url, base: &str, fetched: FetchOutcome) -> PageReport {
        match fetched {
            FetchOutcome::Html { body } => {
                let parsed = parse_html(&body, page_url);
                PageReport {
                    outcome: Outcome::File,
                    extension: extension(page_url),
                    words: parsed.words,
                    children: keep_children(page_url, base, parsed.links),
                }
            }
            FetchOutcome::File => PageReport {
                outcome: Outcome::File,
                extension: extension(page_url),
                words: 0,
                children: Vec::new(),
            },
            FetchOutcome::Redirect { location } => {
                let target = location
                    .and_then(|loc| resolve(page_url, &loc))
                    .map(|u| u.to_string());
                PageReport {
                    outcome: Outcome::Redirect,
                    extension: String::new(),
                    words: 0,
                    children: keep_children(page_url, base, target.into_iter().collect()),
                }
            }
            FetchOutcome::Broken => PageReport {
                outcome: Outcome::Broken,
                extension: String::new(),
                words: 0,
                children: Vec::new(),
            },
        }
    }
}

/// Keep links under the base path, drop the page itself and duplicates (first one wins).
fn keep_children(page_url: &Url, base: &str, links: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for link in links {
        if is_under_base(&link, base) && link != page_url.as_str() && !out.contains(&link) {
            out.push(link);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://h/site/";

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn html_page() {
        let body = r##"<p>Hello world</p>
            <a href="a.html">A</a><a href="a.html#x">A again</a><a href="/other">out</a>
            <a href="">self</a><img src="pic.PNG">"##;
        let r = PageReport::build(&url("http://h/site/"), BASE, FetchOutcome::Html { body: body.into() });
        assert_eq!(r.outcome, Outcome::File);
        assert_eq!(r.extension, "html");
        assert_eq!(r.words, 7); // hello world (2), a (1), a again (2), out (1), self (1)
        assert_eq!(r.children, vec!["http://h/site/a.html", "http://h/site/pic.PNG"]);
    }

    #[test]
    fn same_file_linked_twice_on_a_page_is_one_child() {
        let body = r#"<img src="logo.png"><a href="logo.png">logo</a><img src="./logo.png#x">"#;
        let r = PageReport::build(&url("http://h/site/"), BASE, FetchOutcome::Html { body: body.into() });
        assert_eq!(r.children, vec!["http://h/site/logo.png"]);
    }

    #[test]
    fn extension_comes_from_the_url_not_the_content_type() {
        // A PDF served at a URL with no extension: still a file, extension "html", no words.
        let r = PageReport::build(&url("http://h/site/docs/intro"), BASE, FetchOutcome::File);
        assert_eq!((r.outcome, r.extension.as_str(), r.words), (Outcome::File, "html", 0));
    }

    #[test]
    fn non_html_file() {
        let r = PageReport::build(&url("http://h/site/logo.SVG"), BASE, FetchOutcome::File);
        assert_eq!((r.outcome, r.extension.as_str(), r.words), (Outcome::File, "svg", 0));
        assert!(r.children.is_empty());
    }

    #[test]
    fn redirect_target_becomes_a_child() {
        let r = PageReport::build(
            &url("http://h/site/docs"),
            BASE,
            FetchOutcome::Redirect { location: Some("/site/docs/".into()) },
        );
        assert_eq!(r.outcome, Outcome::Redirect);
        assert_eq!(r.children, vec!["http://h/site/docs/"]);

        let outside = PageReport::build(
            &url("http://h/site/x"),
            BASE,
            FetchOutcome::Redirect { location: Some("http://elsewhere/".into()) },
        );
        assert!(outside.children.is_empty());
    }

    #[test]
    fn broken() {
        let r = PageReport::build(&url("http://h/site/missing.html"), BASE, FetchOutcome::Broken);
        assert_eq!(r.outcome, Outcome::Broken);
        assert!(r.children.is_empty());
    }
}
