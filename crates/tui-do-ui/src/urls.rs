//! Links found in a task, and what to do with one.
//!
//! A URL reaches tui-do in three shapes and the API does not say which to expect: bare in
//! a title or a plain-text description, as a Markdown `[text](url)`, or inside the
//! `href="…"` of the HTML the web editor stores. Rather than parse three grammars, this
//! scans for the scheme and reads to the first character that cannot be in a URL — which
//! happens to terminate all three, because Markdown closes with `)` and HTML with `"`.
//!
//! Measured against the store on 2026-08-31: of 3,878 tasks, 576 carry at least one URL,
//! 462 exactly one, 114 more than one, and the most in a single task is 22. **245 of them
//! are in the *title*, not the description**, which is why both are read.
//!
//! Nothing here opens anything. Opening is a subprocess and therefore an `Effect` the
//! runtime performs — rule 1 — and this module is pure string work.

use tui_do_core::models::Task;

/// Where a link came from, for the picker's dimmed right-hand column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The task's title.
    Title,
    /// The task's description.
    Description,
}

impl Source {
    /// What the picker shows.
    #[must_use]
    pub const fn hint(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Description => "description",
        }
    }
}

/// One link found in a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The URL itself, entity-decoded and stripped of trailing prose punctuation.
    pub url: String,
    /// The Markdown link text, when there was one and it says more than the URL does.
    pub label: Option<String>,
    /// Which field it was found in.
    pub source: Source,
}

/// The schemes worth opening.
///
/// Deliberately short. `mailto:` and `file:` are plausible and both do something
/// surprising when handed to a browser, so they wait until someone wants them.
const SCHEMES: &[&str] = &["https://", "http://"];

/// Characters that cannot appear in a URL and therefore end one.
///
/// `)` and `]` end a Markdown link, `"` and `'` end an HTML attribute, `<` and `>` end an
/// autolink or a tag. That is why no Markdown or HTML parsing is needed here.
/// `)` is deliberately absent. It ends a Markdown link, but it also occurs *inside* real
/// addresses — `…/wiki/Rust_(programming_language)` — and a terminator can only cut a URL
/// short, never restore what it cut. So `)` is read as part of the URL and [`balance`]
/// decides afterwards which ones were the prose's rather than the address's.
const TERMINATORS: &[char] = &['"', '\'', '<', '>', ']', '}', '`', '\\', '|'];

/// Punctuation that ends a sentence rather than a URL.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?'];

/// Every link in a task, title first, in the order they appear, without duplicates.
///
/// Deduplicated by URL because the common HTML shape names the same address twice —
/// `<a href="https://x">https://x</a>` — and offering it to the user twice is noise.
#[must_use]
pub fn extract(task: &Task) -> Vec<Link> {
    let mut found: Vec<Link> = Vec::new();
    scan(&task.title, Source::Title, &mut found);
    scan(&task.description, Source::Description, &mut found);
    found
}

/// Find every link in one field and append the ones not already seen.
fn scan(text: &str, source: Source, found: &mut Vec<Link>) {
    let labels = markdown_labels(text);
    let bytes = text.as_bytes();
    let mut at = 0;

    while at < text.len() {
        let Some(start) = SCHEMES
            .iter()
            .filter_map(|scheme| text[at..].find(scheme).map(|offset| at + offset))
            .min()
        else {
            return;
        };

        let mut end = start;
        while end < text.len() {
            // Walk by character, not byte: a description may hold any UTF-8, and slicing
            // a multi-byte character in half panics.
            let Some(c) = text[end..].chars().next() else {
                break;
            };
            if c.is_whitespace() || c.is_control() || TERMINATORS.contains(&c) {
                break;
            }
            end += c.len_utf8();
        }

        let raw = &text[start..end];
        let trimmed = raw.trim_end_matches(TRAILING);
        // A closing paren belongs to the prose when nothing opened it inside the URL --
        // "(see https://example.com/a)" -- but is part of the address in a Wikipedia-style
        // link. Counting settles it.
        let trimmed = balance(trimmed);

        // Longer than *the scheme that matched here*, not than the shortest scheme there
        // is: with the latter, a bare "https://" in prose is 8 characters against
        // "http://"'s 7 and reads as a link to nowhere.
        let scheme_len = SCHEMES
            .iter()
            .find(|scheme| trimmed.starts_with(**scheme))
            .map_or(usize::MAX, |scheme| scheme.len());

        if trimmed.len() > scheme_len {
            let url = decode_entities(trimmed);
            if !found.iter().any(|link| link.url == url) {
                let label = labels
                    .iter()
                    .find(|(target, _)| *target == url)
                    .map(|(_, text)| text.clone())
                    .filter(|text| text != &url);
                found.push(Link { url, label, source });
            }
        }

        at = if end > start { end } else { start + 1 };
        debug_assert!(at <= bytes.len());
    }
}

/// Drop a trailing `)` that no `(` inside the URL opened.
fn balance(url: &str) -> &str {
    let mut end = url.len();
    loop {
        let candidate = &url[..end];
        if !candidate.ends_with(')') {
            return candidate;
        }
        let opens = candidate.matches('(').count();
        let closes = candidate.matches(')').count();
        if opens >= closes {
            return candidate;
        }
        end -= 1;
    }
}

/// `&amp;` is `&`.
///
/// Vikunja stores HTML, and a query string full of `&amp;` is not the address the user
/// copied. Only the entities that actually turn up in an `href` are handled; a URL
/// containing a literal `&lt;` is not a URL anyone typed.
fn decode_entities(url: &str) -> String {
    url.replace("&amp;", "&")
}

/// The `[text](url)` pairs in this field.
///
/// Only Markdown: an HTML anchor's text is the URL itself in every sample taken from the
/// store, so it would be discarded by the `!= url` filter anyway.
fn markdown_labels(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            return pairs;
        };
        let label = &after[..close];
        let tail = &after[close + 1..];
        if tail.starts_with('(') {
            if let Some(end) = tail.find(')') {
                let target = &tail[1..end];
                if SCHEMES.iter().any(|scheme| target.starts_with(scheme)) {
                    pairs.push((decode_entities(target), label.trim().to_string()));
                }
            }
        }
        rest = &rest[open + 1..];
    }
    pairs
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;

    fn task(title: &str, description: &str) -> Task {
        Task {
            title: title.to_string(),
            description: description.to_string(),
            ..Task::default()
        }
    }

    fn urls(title: &str, description: &str) -> Vec<String> {
        extract(&task(title, description))
            .into_iter()
            .map(|link| link.url)
            .collect()
    }

    #[test]
    fn a_task_with_no_link_yields_none() {
        assert!(urls("Call the VA", "about the case number").is_empty());
        assert!(urls("", "").is_empty());
    }

    #[test]
    fn a_bare_url_in_the_title_is_found() {
        // 245 of the store's tasks carry their URL in the title, not the description.
        assert_eq!(
            urls(
                "https://www.youtube.com/watch?v=aN3X5h9Vfx0&feature=share",
                ""
            ),
            vec!["https://www.youtube.com/watch?v=aN3X5h9Vfx0&feature=share"]
        );
        assert_eq!(
            urls("Synology - http://QuickConnect.to/swaskonas", ""),
            vec!["http://QuickConnect.to/swaskonas"]
        );
    }

    #[test]
    fn a_markdown_link_yields_its_target_and_keeps_its_text() {
        let links = extract(&task(
            "",
            "Original Video: [10 Github Repos That Will Kill Your Monthly Subscriptions]\
             (https://youtu.be/jMAe1h39rHo)",
        ));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://youtu.be/jMAe1h39rHo");
        assert_eq!(
            links[0].label.as_deref(),
            Some("10 Github Repos That Will Kill Your Monthly Subscriptions")
        );
    }

    #[test]
    fn an_html_anchor_yields_its_href_once_not_twice() {
        // The shape the web editor stores names the same address twice, in the attribute
        // and again as the anchor text. Offering both to the user is noise.
        let html = "<p><a target=\"_blank\" rel=\"noopener noreferrer nofollow\" \
                    href=\"https://github.com/nikitabobko/AeroSpace\">\
                    https://github.com/nikitabobko/AeroSpace</a> </p>";
        assert_eq!(
            urls("", html),
            vec!["https://github.com/nikitabobko/AeroSpace"]
        );
    }

    #[test]
    fn an_html_href_has_its_entities_decoded() {
        // A query string full of `&amp;` is not the address anybody copied.
        assert_eq!(
            urls(
                "",
                "<a href=\"https://x.test/a?b=1&amp;c=2&amp;d=3\">go</a>"
            ),
            vec!["https://x.test/a?b=1&c=2&d=3"]
        );
    }

    #[test]
    fn sentence_punctuation_is_not_part_of_the_address() {
        assert_eq!(
            urls("", "See https://example.com/a."),
            vec!["https://example.com/a"]
        );
        assert_eq!(
            urls("", "Try https://example.com/a, then stop"),
            vec!["https://example.com/a"]
        );
        assert_eq!(
            urls("", "(see https://example.com/a)"),
            vec!["https://example.com/a"]
        );
    }

    #[test]
    fn a_paren_the_url_opened_is_kept() {
        assert_eq!(
            urls(
                "",
                "https://en.wikipedia.org/wiki/Rust_(programming_language)"
            ),
            vec!["https://en.wikipedia.org/wiki/Rust_(programming_language)"]
        );
    }

    #[test]
    fn a_long_query_string_survives_intact() {
        // The real Amazon link in the store is ~700 characters of `&`, `=`, `%` and `.`;
        // stopping early would open the wrong page rather than fail visibly.
        let url = "https://www.amazon.com/dp/B0B7S3JSG7/ref=sr_1_1?crid=BPG9MCNTE1BC\
                   &dib=eyJ2IjoiMSJ9.2iYe9TPokHq1xsgdw3FID5oCqJBaVdWbcNIuCGgwBBzB3d0O6\
                   &sr=8-1&th=1";
        assert_eq!(urls("", url), vec![url]);
    }

    #[test]
    fn several_links_come_back_in_order_title_first() {
        let found = urls(
            "https://one.test",
            "then https://two.test and https://three.test",
        );
        assert_eq!(
            found,
            vec!["https://one.test", "https://two.test", "https://three.test"]
        );
    }

    #[test]
    fn the_same_address_twice_is_offered_once() {
        assert_eq!(
            urls("https://one.test", "again: https://one.test"),
            vec!["https://one.test"]
        );
    }

    #[test]
    fn where_a_link_came_from_is_recorded() {
        let links = extract(&task("https://one.test", "https://two.test"));
        assert_eq!(links[0].source, Source::Title);
        assert_eq!(links[1].source, Source::Description);
        assert_eq!(Source::Title.hint(), "title");
    }

    #[test]
    fn a_scheme_with_nothing_after_it_is_not_a_link() {
        assert!(urls("", "https:// and that is all").is_empty());
    }

    #[test]
    fn multibyte_text_around_a_link_does_not_panic() {
        // Descriptions hold anything: the store has em-dashes, CJK and emoji.
        assert_eq!(
            urls("", "日本語 https://example.com/x — 📌 done"),
            vec!["https://example.com/x"]
        );
        assert_eq!(
            urls("", "🔗https://example.com/y"),
            vec!["https://example.com/y"]
        );
    }

    #[test]
    fn a_link_inside_a_markdown_autolink_loses_its_brackets() {
        assert_eq!(
            urls("", "<https://example.com/z>"),
            vec!["https://example.com/z"]
        );
    }
}
