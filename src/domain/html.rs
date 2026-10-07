//! Extracting links and words from an HTML page (docs/DESIGN.md §10.5, §10.6).
//!
//! Synchronous on purpose: `scraper::Html` isn't `Send`, so the parsed document is
//! created and dropped inside this function and never held across an `.await`.

use super::url_rules::resolve;
use super::words::count_words;
use scraper::{Html, Selector};
use url::Url;

pub struct ParsedPage {
    /// Every link on the page, resolved and normalized (may contain duplicates).
    pub links: Vec<String>,
    /// Words in the page's text, excluding tags, attributes, comments,
    /// and the contents of script, style and noscript.
    pub words: u64,
}

const LINK_SELECTOR: &str = "a[href], area[href], link[href], img[src], script[src], \
                             iframe[src], source[src], embed[src], audio[src], video[src]";

pub fn parse_html(body: &str, page_url: &Url) -> ParsedPage {
    let doc = Html::parse_document(body);

    // A <base href="..."> tag changes what relative links are resolved against.
    let base_selector = Selector::parse("base[href]").expect("valid selector");
    let link_base = doc
        .select(&base_selector)
        .next()
        .and_then(|b| b.value().attr("href"))
        .and_then(|href| page_url.join(href.trim()).ok())
        .unwrap_or_else(|| page_url.clone());

    let selector = Selector::parse(LINK_SELECTOR).expect("valid selector");
    let mut links = Vec::new();
    for el in doc.select(&selector) {
        let attr = match el.value().name() {
            "a" | "area" | "link" => "href",
            _ => "src",
        };
        if let Some(value) = el.value().attr(attr) {
            if let Some(url) = resolve(&link_base, value) {
                links.push(url.to_string());
            }
        }
    }

    let mut words = 0;
    for node in doc.root_element().descendants() {
        if let Some(text) = node.value().as_text() {
            let hidden = node.ancestors().any(|a| {
                a.value()
                    .as_element()
                    .is_some_and(|e| matches!(e.name(), "script" | "style" | "noscript"))
            });
            if !hidden {
                words += count_words(text);
            }
        }
    }

    ParsedPage { links, words }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> ParsedPage {
        parse_html(body, &Url::parse("http://h/dir/page.html").unwrap())
    }

    #[test]
    fn collects_href_and_src_links() {
        let p = parse(
            r#"<html><head><link rel="stylesheet" href="s.css"><script src="/app.js"></script></head>
            <body><a href="b.html#x">B</a><img src="i.png"><area href="m.html">
            <a href="mailto:x@y.z">mail</a><a>no href</a><iframe src="f.html"></iframe></body></html>"#,
        );
        assert_eq!(
            p.links,
            vec![
                "http://h/dir/s.css",
                "http://h/app.js",
                "http://h/dir/b.html",
                "http://h/dir/i.png",
                "http://h/dir/m.html",
                "http://h/dir/f.html",
            ]
        );
    }

    #[test]
    fn base_tag_changes_relative_links() {
        let p = parse(r#"<html><head><base href="/other/"></head><body><a href="x.html">X</a></body></html>"#);
        assert_eq!(p.links, vec!["http://h/other/x.html"]);
    }

    #[test]
    fn counts_visible_words_only() {
        let p = parse(
            r#"<html><head><title>Two words</title><style>body { color: red }</style>
            <script>var hidden = "not counted";</script></head>
            <body><p class="ignored attribute">Three more words <!-- a comment here --></p>
            <noscript>skip me</noscript><p>42 isn't</p></body></html>"#,
        );
        // "two words" + "three more words" + "isn", "t" = 2 + 3 + 2
        assert_eq!(p.words, 7);
    }
}
