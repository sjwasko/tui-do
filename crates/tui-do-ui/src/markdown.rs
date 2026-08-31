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
/// One entry per tag, not `"<p>"` and `"<p "` as two: the trailing-character guard below
/// already tells `<p>` and `<p dir="ltr">` from `<pre-release plan>`, so a tag needs only
/// its opening form. `"<p "` was dead code before this list was trimmed to one entry each
/// — it can only match two literal spaces — and its absence let `<p dir="ltr">…</p>`
/// (TipTap's text-align extension emits `<p style="text-align: center">`) fall through to
/// the markdown branch and show literal tags on screen. Nothing else in the list carried
/// the same trailing-character mistake; only `<p>`/`<p ` was ever split in two.
const HTML_BLOCKS: &[&str] = &[
    "<p",
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

/// The closing form of each `HTML_BLOCKS` entry that has one.
///
/// `<br>` is void and closes nothing, so it is the one tag missing here. Everything else
/// pairs an opening and a closing tag because TipTap always emits both: it wraps its whole
/// document in block elements and never emits an unclosed one. Prose that merely mentions
/// markup supplies at most one half — the open or the close, never reliably both — which is
/// the pair `looks_like_html` tests for.
const HTML_CLOSING_TAGS: &[&str] = &[
    "</p>",
    "</h1>",
    "</h2>",
    "</h3>",
    "</h4>",
    "</h5>",
    "</h6>",
    "</ul>",
    "</ol>",
    "</li>",
    "</pre>",
    "</table>",
    "</div>",
    "</blockquote>",
];

/// Whether this description came out of the web editor rather than a keyboard.
///
/// The test used to be "contains a recognised block tag anywhere", and that is not strong
/// enough: `"Wrap it in a <div> and add the class."` contains `<div>` and is prose, and
/// routing it to the HTML branch silently deleted the word — `html5ever` reads `<div>` as
/// a real element and the surrounding text as its (empty) content. The same happened with
/// `<li>`, `<table>` and `<pre>` mentioned in a sentence.
///
/// The fix after that ("starts with a block tag, OR starts with `<` and closes one
/// anywhere") caused a regression of its own: ids 49, 369 and 420 on dev are genuine
/// TipTap output — a leading sentence of bare text (`"yay -S naps2-bin"`) followed by real
/// `<div><br></div>` blocks, the shape pasting from Google Keep into TipTap produces — and
/// none of them *opens* on `<`, so anchoring the closing-tag check to "starts with `<`"
/// routed all three away from HTML they genuinely are.
///
/// **The discriminating fact is not position, it is the pair.** A genuine TipTap document
/// carries both halves of at least one element — `<div>` and `</div>`, or `<p>` and
/// `</p>` — because TipTap always closes what it opens, wherever in the string that
/// happens to fall. Prose that merely mentions markup supplies at most one half at a
/// time: `"Wrap it in a <div> and add the class."` has the open with no close;
/// `"Remember to close the </div> tag at the end."` has the close with no open. Neither
/// sentence needs the other half to make its point, and neither one accidentally has it.
/// So the rule drops the "starts with `<`" anchor entirely and asks a stronger question
/// instead: does the string contain *both* an opening tag and *its own* matching closing
/// tag, for the same element, anywhere?
///
/// A leading block tag is still checked on its own (`starts_with_block_tag`), because a
/// document can open on `<p>` with no `</p>` yet typed — mid-edit, or truncated — and that
/// is still real HTML with nothing to pair against.
///
/// The pairing subsumes the old `<img>`-first case for free: ids 27 and 116
/// (`<img …><p></p>`, see `an_image_only_description_renders_nothing`) don't open on a
/// recognised block tag, but they carry a bare `<p>` immediately followed by `</p>`, which
/// the pairing check finds regardless of what came before it.
///
/// **Known residual, accepted rather than chased:** a sentence that names *both* halves —
/// `"use <div> and close with </div>"` — still misroutes to HTML and loses both words.
/// Nothing short of parsing the sentence tells that apart from a real TipTap paste of an
/// empty `<div></div>`, and the two previous rounds already show where chasing an ever
/// narrower discriminator on string shape alone leads.
///
/// Validated against the 487 real descriptions on dev: the previous rule counted 108 as
/// HTML; this rule counts 112. Four rows moved, all from Markdown to HTML: ids 49, 369 and
/// 420 (the regression this fix targets), plus id 1842, not previously known — free-form
/// notes with a genuine `<p>…</p>` fragment (and several `<br />`) pasted into the middle
/// of otherwise plain text. That row does carry a real matching pair, so the rule is
/// working as specified; it was not one of the cases this fix was written against. Row
/// 417 (`"...waits<br>...drifts...<br>..."`, no wrapping element, `<br>` has no closing
/// tag to pair) stays Markdown, as before.
pub(crate) fn looks_like_html(description: &str) -> bool {
    let trimmed = description.trim();
    let lower = trimmed.to_ascii_lowercase();
    starts_with_block_tag(&lower) || contains_matching_pair(&lower)
}

/// Whether `lower` (already trimmed and lowercased) opens with a recognised block tag.
fn starts_with_block_tag(lower: &str) -> bool {
    HTML_BLOCKS.iter().any(|tag| {
        lower.strip_prefix(tag).is_some_and(|rest| {
            // `<pre-release plan>` starts with `<pre`, and is prose. A real tag is
            // followed by `>`, whitespace, or `/`; a hyphenated word is not.
            rest.chars()
                .next()
                .is_none_or(|c| c == '>' || c == '/' || c.is_whitespace())
        })
    })
}

/// Whether `lower` contains, anywhere, both an opening tag from `HTML_BLOCKS` and the
/// matching closing tag from `HTML_CLOSING_TAGS` for the *same* element. `<br>` has no
/// closing form and is excluded by `zip` stopping at the shorter list.
fn contains_matching_pair(lower: &str) -> bool {
    HTML_BLOCKS
        .iter()
        .zip(HTML_CLOSING_TAGS.iter())
        .any(|(open, close)| contains_tag_open(lower, open) && lower.contains(close))
}

/// Whether `lower` contains an opening `tag` (e.g. `"<div"`) anywhere, not just at the
/// front — unlike `starts_with_block_tag`, this looks for a pasted block that follows a
/// leading sentence of prose. The same trailing-character guard applies at each
/// occurrence: `<div-shaped>` does not count, wherever it falls.
fn contains_tag_open(lower: &str, tag: &str) -> bool {
    lower.match_indices(tag).any(|(start, _)| {
        lower[start + tag.len()..]
            .chars()
            .next()
            .is_none_or(|c| c == '>' || c == '/' || c.is_whitespace())
    })
}

use html2text::render::TextDecorator;
use ratatui::style::{Modifier, Style};

use crate::theme::Theme;

/// How each construct is painted.
///
/// `TextDecorator::Annotation` is whatever the implementor says it is, so it is a ratatui
/// `Style` and there is no second palette to keep in step with `theme.rs` — which is the
/// thing `glow` could not offer at any price.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deco {
    theme: Theme,
}

impl TextDecorator for Deco {
    type Annotation = Style;

    fn decorate_link_start(&mut self, _url: &str) -> (String, Style) {
        (
            String::new(),
            self.theme.accent().add_modifier(Modifier::UNDERLINED),
        )
    }

    fn decorate_link_end(&mut self) -> String {
        String::new()
    }

    fn decorate_em_start(&self) -> (String, Style) {
        (
            String::new(),
            self.theme.text().add_modifier(Modifier::ITALIC),
        )
    }

    fn decorate_em_end(&self) -> String {
        String::new()
    }

    fn decorate_strong_start(&self) -> (String, Style) {
        (
            String::new(),
            self.theme.text().add_modifier(Modifier::BOLD),
        )
    }

    fn decorate_strong_end(&self) -> String {
        String::new()
    }

    fn decorate_strikeout_start(&self) -> (String, Style) {
        (
            String::new(),
            self.theme.text().add_modifier(Modifier::CROSSED_OUT),
        )
    }

    fn decorate_strikeout_end(&self) -> String {
        String::new()
    }

    // Foreground only: the theme paints no backgrounds, deliberately -- see theme.rs.
    fn decorate_code_start(&self) -> (String, Style) {
        (String::new(), self.theme.accent())
    }

    fn decorate_code_end(&self) -> String {
        String::new()
    }

    fn decorate_preformat_first(&self) -> Style {
        self.theme.muted()
    }

    fn decorate_preformat_cont(&self) -> Style {
        self.theme.muted()
    }

    fn decorate_image(&mut self, _src: &str, title: &str) -> (String, Style) {
        (format!("[{title}]"), self.theme.muted())
    }

    fn header_prefix(&self, level: usize) -> String {
        format!("{} ", "#".repeat(level))
    }

    fn push_colour(&mut self, colour: html2text::Colour) -> Option<Style> {
        Some(
            self.theme
                .text()
                .fg(ratatui::style::Color::Rgb(colour.r, colour.g, colour.b)),
        )
    }

    fn pop_colour(&mut self) -> bool {
        true
    }

    fn quote_prefix(&self) -> String {
        "│ ".to_string()
    }

    fn unordered_item_prefix(&self) -> String {
        "• ".to_string()
    }

    fn ordered_item_prefix(&self, i: i64) -> String {
        format!("{i}. ")
    }

    fn make_subblock_decorator(&self) -> Self {
        *self
    }
}

use ratatui::text::{Line, Span};

/// The narrowest pane worth rendering into.
///
/// `html2text` is given a width and a one- or two-column pane has nothing useful to say;
/// asking it to wrap into that is how a renderer ends up in a loop or a panic.
const FLOOR: u16 = 4;

/// The theme, as the few CSS rules `html2text` can act on.
///
/// `TextDecorator` has `header_prefix` but no header *annotation*, so this is the only
/// way a heading gets a colour. Anything not listed here is painted by the decorator.
///
/// Each declaration ends in `;`, deliberately: `html2text`'s CSS parser (measured against
/// 0.17.1) silently drops a block's last declaration when it has no trailing semicolon, so
/// `color: #rrggbb }` with no `;` parses as an empty ruleset and every heading comes back
/// uncoloured with no error at all.
fn agent_css(theme: Theme) -> String {
    let Some(rgb) = style_rgb(theme.accent()) else {
        return String::new();
    };
    let muted = style_rgb(theme.muted()).unwrap_or(rgb);
    format!(
        "h1,h2,h3,h4,h5,h6 {{ color: #{:02x}{:02x}{:02x}; }}\n\
         blockquote {{ color: #{:02x}{:02x}{:02x}; }}\n",
        rgb.0, rgb.1, rgb.2, muted.0, muted.1, muted.2
    )
}

/// The RGB of a style's foreground, when it has one that CSS can name.
///
/// Only `Color::Rgb` matches. `Shade::at` (theme.rs) answers `Color::Blue` at Ansi16 and
/// `Color::Indexed` at Ansi256, and CSS has no way to name either, so `agent_css` falls
/// back to an empty stylesheet and a heading keeps `header_prefix`'s `#` with no colour.
/// Deliberate: a non-TrueColor terminal already renders every other themed element in its
/// approximation of the theme, and an uncoloured heading is the honest result of asking a
/// hex-only stylesheet to describe a 16- or 256-colour one, not a bug to chase.
fn style_rgb(style: Style) -> Option<(u8, u8, u8)> {
    match style.fg {
        Some(ratatui::style::Color::Rgb(r, g, b)) => Some((r, g, b)),
        _ => None,
    }
}

/// A task's description, ready to draw.
///
/// `width` is columns, `theme` decides every colour. Answers an empty vector for an empty
/// description and for anything the pipeline cannot make sense of — a description is not
/// worth failing the frame over.
#[must_use]
pub fn render(description: &str, width: u16, theme: Theme) -> Vec<Line<'static>> {
    if description.trim().is_empty() {
        return Vec::new();
    }

    let html = if looks_like_html(description) {
        description.to_string()
    } else {
        let mut options = comrak::Options::default();
        // A single newline is a line break. Deliberately not CommonMark: roughly 350 of
        // the 367 tag-free descriptions on dev carry their structure in single newlines,
        // and conforming here runs an address block together into one line.
        options.render.hardbreaks = true;
        // Text that looks like a tag stays text. Without this `<String>` in
        // `Use Vec<String> here` is read as raw HTML and dropped, silently.
        options.render.escape = true;
        options.extension.table = true;
        options.extension.strikethrough = true;
        options.extension.autolink = true;
        comrak::markdown_to_html(description, &options)
    };

    let config = html2text::config::with_decorator(Deco { theme });
    let config = match config.add_agent_css(&agent_css(theme)) {
        Ok(config) => config,
        // A stylesheet this module wrote failing to parse is a bug here, not bad input,
        // and it costs colour rather than correctness -- render without it.
        Err(_) => html2text::config::with_decorator(Deco { theme }),
    };
    let Ok(lines) = config.lines_from_read(html.as_bytes(), usize::from(width.max(FLOOR))) else {
        return Vec::new();
    };

    let mut rendered: Vec<Line<'static>> = lines
        .iter()
        .map(|line| {
            Line::from(
                line.tagged_strings()
                    .map(|tagged| {
                        // Annotations nest, outermost first, so `<strong><em>` arrives as
                        // both and patches into one style.
                        let style = tagged
                            .tag
                            .iter()
                            .fold(Style::new(), |acc, next| acc.patch(*next));
                        Span::styled(tagged.s.clone(), style)
                    })
                    .collect::<Vec<Span<'static>>>(),
            )
        })
        .collect();

    // `html2text` can hand back a trailing blank line for what was, semantically, an empty
    // document -- filter those off rather than call it content.
    while rendered
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
    {
        rendered.pop();
    }

    rendered
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::theme::{ColorDepth, Theme};
    use html2text::render::TextDecorator;
    use ratatui::style::Modifier;
    use ratatui::text::Line;

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn rendered(description: &str, width: u16) -> Vec<String> {
        text_of(&render(
            description,
            width,
            Theme::new(ColorDepth::TrueColor),
        ))
    }

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
        // FIX 2: `<p dir="ltr">` and `<p style="text-align: center">` (TipTap's
        // text-align extension) both open with `<p` followed by whitespace, not `>` or a
        // second space -- the dead `"<p "` entry this list used to carry never matched
        // either shape.
        assert!(looks_like_html("<p dir=\"ltr\">left to right</p>"));
        assert!(looks_like_html(
            "<p style=\"text-align: center\">centered</p>"
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
    fn prose_that_merely_mentions_a_block_tag_is_not_html() {
        // FIX 1: the pre-fix rule matched a bare mention of a tag anywhere in the string,
        // which routed every one of these to the HTML branch and deleted the bracketed
        // word -- `html5ever` reads `<div>` as a real, empty element. Each of these has
        // the opening half of a tag and not the matching close, which is what now tells
        // them apart from genuine TipTap output -- see `contains_matching_pair`.
        assert!(!looks_like_html("Wrap it in a <div> and add the class."));
        assert!(!looks_like_html("Add an <li> element for each row."));
        assert!(!looks_like_html("Use a <table> to lay it out."));
        assert!(!looks_like_html("A <pre> block preserves whitespace."));
    }

    #[test]
    fn prose_that_describes_closing_a_tag_is_not_html() {
        // FIX 3, still true under the pairing rule (FIX 4): a sentence that only
        // describes closing a tag has the closing half with no matching open, so it
        // fails `contains_matching_pair` exactly as the opening-only case above does.
        // None of these carries both halves of the same element.
        assert!(!looks_like_html(
            "Remember to close the </div> tag at the end."
        ));
        assert!(!looks_like_html(
            "Don't forget </p> at the end of your HTML snippet."
        ));
        assert!(!looks_like_html(
            "The template ends with </table> not </div>."
        ));
    }

    #[test]
    fn prose_that_describes_closing_a_tag_survives_the_round_trip() {
        // The rendering half of FIX 3: not just that these are classified as text, but
        // that the words `html5ever` would otherwise have swallowed actually stay on
        // screen.
        assert_eq!(
            rendered("Remember to close the </div> tag at the end.", 60),
            vec!["Remember to close the </div> tag at the end."]
        );
        assert_eq!(
            rendered("Don't forget </p> at the end of your HTML snippet.", 60),
            vec!["Don't forget </p> at the end of your HTML snippet."]
        );
        assert_eq!(
            rendered("The template ends with </table> not </div>.", 60),
            vec!["The template ends with </table> not </div>."]
        );
    }

    #[test]
    fn bare_br_with_no_wrapping_element_is_not_html() {
        // Description id 417 on dev, title "test task": three lines joined by `<br>`
        // with no surrounding block element. Not TipTap's output -- the web editor always
        // wraps -- so it is treated as text and the literal `<br>` stays visible rather
        // than being read as three real line breaks. Recorded because the pre-fix rule
        // got this one right by the accident of matching `<br>` on its own, and this is
        // the one row in the 487-description corpus whose routing changed.
        assert!(!looks_like_html(
            "Empty inbox waits<br>A test task drifts into view<br>Proof the pipe still flows"
        ));
    }

    #[test]
    fn a_pasted_div_block_after_leading_text_is_html() {
        // FIX 4: ids 49, 369 and 420 on dev are this exact shape -- a leading sentence of
        // bare text (e.g. `"yay -S naps2-bin"`) followed by real `<div><br></div>` blocks,
        // the shape pasting from Google Keep into TipTap produces. The FIX-3 rule anchored
        // the closing-tag search to "starts with `<`", which these don't, and misrouted
        // all three to Markdown -- a regression, since they used to render properly as
        // HTML. `<div>` and `</div>` both appear here, which is what the pairing rule
        // looks for regardless of what came before them.
        assert!(looks_like_html("yay -S naps2-bin<div><br></div>"));
        assert!(looks_like_html(
            "Some notes here first.<div>then a block</div><div><br></div>"
        ));
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

    fn deco() -> Deco {
        Deco {
            theme: Theme::new(ColorDepth::TrueColor),
        }
    }

    #[test]
    fn the_decorator_answers_in_the_projects_own_theme() {
        let theme = Theme::new(ColorDepth::TrueColor);
        let mut d = deco();
        assert_eq!(
            d.decorate_strong_start().1,
            theme.text().add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            d.decorate_em_start().1,
            theme.text().add_modifier(Modifier::ITALIC)
        );
        assert_eq!(d.decorate_code_start().1, theme.accent());
        assert_eq!(
            d.decorate_link_start("http://x").1,
            theme.accent().add_modifier(Modifier::UNDERLINED)
        );
        assert_eq!(d.decorate_preformat_first(), theme.muted());
    }

    #[test]
    fn list_and_quote_prefixes_are_the_ones_the_preview_pane_already_uses() {
        let d = deco();
        // `rows::plain_text` used "• " for a list item; the preview should not change
        // shape just because the renderer under it did.
        assert_eq!(d.unordered_item_prefix(), "• ");
        assert_eq!(d.ordered_item_prefix(3), "3. ");
        assert_eq!(d.quote_prefix(), "│ ");
    }

    #[test]
    fn an_empty_description_renders_nothing() {
        assert!(render("", 40, Theme::new(ColorDepth::TrueColor)).is_empty());
        assert!(render("   \n  \n", 40, Theme::new(ColorDepth::TrueColor)).is_empty());
    }

    #[test]
    fn a_single_newline_is_a_line_break() {
        // THE REGRESSION THIS FEATURE EXISTS TO AVOID. CommonMark joins consecutive
        // lines into one paragraph, which turns this real description's address block
        // into "Bradenton - old historic courthouse 1115 Manatee Ave West 830a-430p".
        // `comrak`'s render.hardbreaks is what stops it. If someone "fixes" that to be
        // spec-conforming, this test is what tells them what they broke.
        let description = "Manatee County:\n\
                           Bradenton - old historic courthouse\n\
                           1115 Manatee Ave West\n\
                           830a-430p";
        let lines = rendered(description, 60);
        assert!(lines.iter().any(|l| l.contains("1115 Manatee Ave West")));
        assert!(
            !lines.iter().any(|l| l.contains("courthouse 1115")),
            "the address block collapsed into one line: {lines:?}"
        );
    }

    #[test]
    fn prose_containing_angle_brackets_survives_the_round_trip() {
        // THE OTHER REGRESSION. Without comrak's render.escape, `<String>` is read as raw
        // inline HTML, replaced with an omitted-HTML comment, and dropped -- the line
        // comes out "Use Vec here". It fails silently, by deleting text.
        assert_eq!(
            rendered("Use Vec<String> here", 60),
            vec!["Use Vec<String> here"]
        );
        assert_eq!(rendered("a < b and b > c", 60), vec!["a < b and b > c"]);
        assert_eq!(rendered("a <b", 60), vec!["a <b"]);
        assert_eq!(
            rendered("<pre-release plan>", 60),
            vec!["<pre-release plan>"]
        );
        assert_eq!(
            rendered("# - CAM_<CAMERA_MAC>_NAME=fr", 60),
            vec!["# - CAM_<CAMERA_MAC>_NAME=fr"]
        );
    }

    #[test]
    fn entities_in_stored_html_decode_once() {
        // Carried across from `rows::plain_text`. `&#x27;` is what the dev instance
        // stores; `&#39;` and `&#34;` are what Go's html.EscapeString emits.
        assert_eq!(
            rendered("<p>Tom &amp; Jerry &lt;3</p>", 60),
            vec!["Tom & Jerry <3"]
        );
        assert_eq!(
            rendered("<p>&amp;lt; stays escaped</p>", 60),
            vec!["&lt; stays escaped"]
        );
        assert_eq!(
            rendered("<p>TechHut&#x27;s homelab</p>", 60),
            vec!["TechHut's homelab"]
        );
        assert_eq!(
            rendered("<p>say &#34;hello&#34;</p>", 60),
            vec!["say \"hello\""]
        );
        assert_eq!(rendered("<p>an &#8212; dash</p>", 60), vec!["an — dash"]);
    }

    #[test]
    fn an_autolink_loses_its_brackets_and_gains_a_style() {
        // A deliberate change from `plain_text`, which kept the brackets: CommonMark
        // reads `<http://…>` as an autolink, so the text is the URL and it is styled as
        // a link. Recorded because it is a *change*, and because URL opening later in
        // Phase 5 wants exactly this.
        let theme = Theme::new(ColorDepth::TrueColor);
        let lines = render("<http://Www.ftc.gov> Report fraud", 60, theme);
        assert_eq!(text_of(&lines), vec!["http://Www.ftc.gov Report fraud"]);
        let link = theme.accent().add_modifier(Modifier::UNDERLINED);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style == link && s.content.contains("ftc.gov")),
            "the URL was not styled as a link: {:?}",
            lines[0].spans
        );
    }

    #[test]
    fn html_from_the_web_editor_renders_its_structure() {
        let html = "<p>The refresh call needs the cookie set by \
                    <a target=\"_blank\" rel=\"noopener\" href=\"http://x\">POST /login</a>.</p>\
                    <ul><li>one</li><li>two</li></ul>";
        let lines = rendered(html, 60);
        assert!(lines.iter().any(|l| l.contains("POST /login")));
        assert!(lines.iter().any(|l| l.contains("• one")));
        assert!(lines.iter().any(|l| l.contains("• two")));
    }

    #[test]
    fn markdown_constructs_are_styled_not_just_kept() {
        let theme = Theme::new(ColorDepth::TrueColor);
        let lines = render("**bold** and *em* and `code`", 60, theme);
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let has = |style: ratatui::style::Style, text: &str| {
            spans
                .iter()
                .any(|s| s.style == style && s.content.contains(text))
        };
        assert!(has(theme.text().add_modifier(Modifier::BOLD), "bold"));
        assert!(has(theme.text().add_modifier(Modifier::ITALIC), "em"));
        assert!(has(theme.accent(), "code"));
    }

    #[test]
    fn long_lines_wrap_to_the_width_they_are_given() {
        let long = "Hillsborough County Clerk of Court, Family Law division, room 101";
        for line in rendered(long, 30) {
            assert!(
                crate::rows::display_width(&line) <= 30,
                "line over width: {line:?}"
            );
        }
        assert!(rendered(long, 30).len() > 1, "nothing wrapped at width 30");
    }

    #[test]
    fn a_width_too_small_to_render_in_does_not_panic() {
        for width in [0, 1, 2, 3] {
            let _ = render(
                "some text that cannot fit",
                width,
                Theme::new(ColorDepth::TrueColor),
            );
        }
    }

    #[test]
    fn a_heading_is_coloured_not_merely_prefixed() {
        let theme = Theme::new(ColorDepth::TrueColor);
        let lines = render("# Plan: Migrate Forgejo\n\nbody text", 60, theme);
        let heading = lines
            .iter()
            .find(|l| text_of(std::slice::from_ref(l))[0].contains("Migrate Forgejo"))
            .expect("no heading line");
        assert!(
            heading
                .spans
                .iter()
                .any(|s| s.style != Style::new() && s.style != theme.text()),
            "the heading was not styled: {:?}",
            heading.spans
        );
    }

    #[test]
    fn a_trailing_empty_paragraph_is_not_content() {
        // Vikunja's web editor genuinely produces this shape: an empty paragraph left
        // behind at the end of a description, holding only a non-breaking space.
        let lines = rendered("<p>real text</p><p>&nbsp;</p>", 60);
        assert!(
            lines.last().is_some_and(|l| l.contains("real text")),
            "a trailing blank line was kept as content: {lines:?}"
        );
    }

    #[test]
    fn script_and_style_content_is_not_shown() {
        // `rows::plain_text` had an `OPAQUE` list for exactly this: `<script>` and
        // `<style>` hold code and CSS, not prose, and a user must never see either in a
        // task's description. Measured: `html2text` (via `html5ever`) drops both element
        // bodies on its own, with no help from this module.
        let lines = rendered(
            "<p>real text</p><script>alert('x')</script><style>p{color:red}</style>",
            60,
        );
        let joined = lines.join("\n");
        assert!(
            lines.iter().any(|l| l.contains("real text")),
            "the real content went missing too: {lines:?}"
        );
        assert!(
            !joined.contains("alert"),
            "script content leaked into the rendered description: {lines:?}"
        );
        assert!(
            !joined.contains("color:red") && !joined.contains("color: red"),
            "style content leaked into the rendered description: {lines:?}"
        );
    }

    #[test]
    fn table_cells_are_separated_rather_than_run_together() {
        // Decision 6 turns on `extension.table = true`; nothing asserted its output
        // survived the trip through `html2text`. Measured: both a Markdown pipe table
        // and a raw `<table>` come out as a boxed grid with `│` between cells and `┬ ─ ┼
        // ┴` for the rules, not text run together.
        let md_lines = rendered("| a | b |\n|---|---|\n| 1 | 2 |", 60);
        let md_joined = md_lines.join("\n");
        assert!(
            md_lines.iter().any(|l| l.contains('│')),
            "markdown table cells were not visibly separated: {md_lines:?}"
        );
        assert!(
            !md_joined.contains("ab") && !md_joined.contains("12"),
            "markdown table cells ran together: {md_lines:?}"
        );

        let html = "<table><tr><td>one</td><td>two</td></tr><tr><td>three</td></tr></table>";
        let html_lines = rendered(html, 60);
        let row = html_lines
            .iter()
            .find(|l| l.contains("one"))
            .expect("no rendered row held the first cell");
        assert!(
            row.contains("two"),
            "the two cells of one row did not land on the same line: {html_lines:?}"
        );
        assert!(
            !row.contains("onetwo"),
            "html table cells ran together: {html_lines:?}"
        );
        assert!(
            row.contains('│'),
            "html table cells had no visible separator: {html_lines:?}"
        );
    }

    #[test]
    fn an_image_only_description_renders_nothing() {
        // Store ids 27 and 116 on dev, both this exact shape: an `<img>` TipTap left
        // outside any paragraph, followed by the empty paragraph it leaves behind. Not a
        // regression -- `rows::plain_text` showed nothing for these two either -- and not
        // fixed here: what an image should look like in a terminal is its own question.
        let html = "<img data-src=\"https://example/attachments/1\" src=\"#\" \
                    id=\"tiptap-image-1\"><p></p>";
        assert!(rendered(html, 60).is_empty());
    }
}
