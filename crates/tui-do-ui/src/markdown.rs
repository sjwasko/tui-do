//! Task descriptions, rendered.
//!
//! A description is Markdown, plain text, or the HTML Vikunja's web editor stores, and
//! which of the three is not knowable from the API — see `CLAUDE.md`. Markdown and plain
//! text are rendered to HTML by `comrak`; HTML is already HTML. Both then go through
//! `html2text`, which wraps to a width and hands back lines annotated with the styles this
//! module chose, one span per styled run.
//!
//! Nothing here spawns a process or touches I/O, so rule 1 is unengaged rather than merely
//! respected: `update` gains no `Effect`, the model gains no cache.

/// Elements that only appear in generated HTML, never in prose.
///
/// The test is deliberately *block* tags and not "contains a `<`". Real descriptions say
/// `Use Vec<String> here` and `# - CAM_<CAMERA_MAC>_NAME=fr`, and sending those down the
/// HTML branch deletes the bracketed word — `html5ever` reads it as an unknown element and
/// drops it. TipTap always wraps its content in at least one of these.
// Task 4 calls this; `expect` rather than `allow` so that wiring it up makes this
// attribute warn and forces its own removal.
#[cfg_attr(not(test), expect(dead_code))]
#[cfg_attr(test, allow(dead_code))]
const HTML_BLOCKS: &[&str] = &[
    "<p>",
    "<p ",
    "<h1",
    "<h2",
    "<h3",
    "<h4",
    "<h5",
    "<h6",
    "<ul",
    "<ol",
    "<li",
    "<pre",
    "<table",
    "<div",
    "<blockquote",
    "<br",
];

/// Whether this description came out of the web editor rather than a keyboard.
// Task 4 calls this; `expect` rather than `allow` so that wiring it up makes this
// attribute warn and forces its own removal.
#[cfg_attr(not(test), expect(dead_code))]
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn looks_like_html(description: &str) -> bool {
    let lower = description.to_ascii_lowercase();
    HTML_BLOCKS.iter().any(|tag| {
        lower.match_indices(tag).any(|(at, _)| {
            // `<pre-release plan>` starts with `<pre`, and is prose. A real tag is
            // followed by `>`, whitespace, or `/`; a hyphenated word is not.
            let after = at + tag.len();
            tag.ends_with('>')
                || lower[after..]
                    .chars()
                    .next()
                    .is_none_or(|c| c == '>' || c == '/' || c.is_whitespace())
        })
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn html_from_the_web_editor_is_recognised() {
        assert!(looks_like_html("<p>Install the Wyze bridge in docker</p>"));
        assert!(looks_like_html(
            "<h1>Hermes Report</h1><p><strong>Date:</strong> 2026-07-03</p>"
        ));
        assert!(looks_like_html("<ul><li>one</li><li>two</li></ul>"));
        assert!(looks_like_html(
            "<pre><code>1. Enable RTSP streams</code></pre>"
        ));
        // Attributes, which TipTap always emits.
        assert!(looks_like_html(
            "<p><a target=\"_blank\" rel=\"noopener\" href=\"http://x\">POST /login</a></p>"
        ));
    }

    #[test]
    fn prose_that_contains_angle_brackets_is_not_html() {
        // Every one of these is a real description on the dev instance, carried across
        // from `rows::plain_text`'s tests. Routing any of them to the HTML branch deletes
        // the bracketed word.
        assert!(!looks_like_html("Use Vec<String> here"));
        assert!(!looks_like_html("# - CAM_<CAMERA_MAC>_NAME=fr"));
        assert!(!looks_like_html("a < b and b > c"));
        assert!(!looks_like_html("a <b"));
        assert!(!looks_like_html("<pre-release plan>"));
        assert!(!looks_like_html("<http://Www.ftc.gov> Report fraud"));
    }

    #[test]
    fn plain_text_and_markdown_are_not_html() {
        assert!(!looks_like_html(""));
        assert!(!looks_like_html("Call the VA about the case number"));
        assert!(!looks_like_html(
            "# Plan: Migrate Forgejo\n\n## Overview\n\n- one\n- two"
        ));
        assert!(!looks_like_html("| a | b |\n|---|---|\n| 1 | 2 |"));
    }
}
