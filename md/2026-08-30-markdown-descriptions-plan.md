# Markdown task descriptions — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render a task's description in the preview pane as styled, wrapped text — headings,
emphasis, code, links, lists, quotes — whether the description is Markdown, plain text, or
the HTML Vikunja's web editor stores.

**Architecture:** A description takes one of two branches. If it contains a recognised HTML
block tag it goes straight into `html2text`; otherwise `comrak` renders it to HTML first.
Both land in `html2text` driven by a `TextDecorator` whose `Annotation` is a
`ratatui::style::Style`, which yields lines already wrapped to the pane width, with one span
per styled run. Nothing spawns a subprocess, so `update` is untouched: no `Effect`, no `Msg`,
no model state, no cache.

**Tech Stack:** Rust, ratatui 0.30, `comrak` 0.54 (Markdown → HTML), `html2text` 0.17
(HTML → styled lines), `html5ever` (underneath `html2text`).

**Spec:** `md/2026-08-30-markdown-descriptions-design.md` — read it first. It records what
was measured and why each option was rejected; this plan does not repeat the arguments.

## Global Constraints

- **Rule 1 (`CLAUDE.md`):** `tui-do-ui` is pure and synchronous. Nothing in this plan may
  add `.await`, `spawn_blocking`, a `Store` call, or a dependency on `reqwest`/`rusqlite`/
  `tokio`. The test `a_pure_ui_names_no_io` in `crates/tui-do-ui/src/lib.rs` enforces this.
- **Workspace lints deny `unwrap`, `panic`, `todo`, `dbg!` and forbid `unsafe`** in
  production code. Test modules may `allow` them at module level — follow the existing
  `#![allow(...)]` pattern in `rows.rs`'s test module.
- **`cargo clippy --workspace --all-targets` must be clean; CI runs with `-D warnings`.**
- **`cargo fmt --all`** before every commit.
- Dependencies are declared in the root `Cargo.toml` under `[workspace.dependencies]` and
  referenced as `name.workspace = true` in the member crate. Follow that pattern exactly.
- **The theme paints foregrounds and selections only, never backgrounds** (`theme.rs:76-77`).
  Code spans get a foreground colour, not a background.
- Exact versions: `comrak = { version = "0.54", default-features = false }`,
  `html2text = { version = "0.17", features = ["css"] }`.

---

## File Structure

| file | responsibility |
|---|---|
| `crates/tui-do-ui/src/markdown.rs` | **new.** The whole feature: the router, the `Deco` decorator, and `render()`. One module because the three parts are meaningless apart and total ~200 lines. |
| `crates/tui-do-ui/src/rows.rs` | `plain_text` and its helpers deleted (lines 602–924). `display_width`, `truncate`, `wrap`, `relative_date` and everything above line 602 stay — they serve the task list, not the preview. |
| `crates/tui-do-ui/src/view.rs` | `preview()` calls `markdown::render` instead of `rows::plain_text`. |
| `crates/tui-do-ui/src/lib.rs` | `pub mod markdown;` |
| `Cargo.toml`, `crates/tui-do-ui/Cargo.toml` | add `comrak`, `html2text`; remove the unused `pulldown-cmark`. |
| `PLAN.md`, `CLAUDE.md` | documentation, Task 8. |

---

### Task 1: Dependencies, and remove the one that was never used

**Files:**
- Modify: `Cargo.toml` (the `[workspace.dependencies]` table, around line 42)
- Modify: `crates/tui-do-ui/Cargo.toml:18`

**Interfaces:**
- Consumes: nothing.
- Produces: the crates `comrak` and `html2text` available to `tui-do-ui`.

`pulldown-cmark` was declared by the first scaffold commit (`d63357f`, when the project was
called `criax`) in anticipation of this feature and no source file has ever imported it.
comrak replaces it, so it goes.

- [ ] **Step 1: Confirm `pulldown-cmark` really is unused**

```bash
grep -rn "pulldown_cmark" crates/ --include=*.rs
```

Expected: no output. If anything is printed, STOP — the spec's claim is wrong and this
task needs rethinking.

- [ ] **Step 2: Edit the workspace dependency table**

In `Cargo.toml`, replace the line

```toml
pulldown-cmark = { version = "0.13", default-features = false }
```

with

```toml
# Markdown -> HTML. `default-features = false` drops the CLI-only extras; the parser and
# `markdown_to_html` are in the default-off set.
comrak = { version = "0.54", default-features = false }
# HTML -> wrapped, annotated lines. The `css` feature is what lets a user-agent stylesheet
# colour headings, which the TextDecorator trait alone cannot do -- see the design's
# decision 5.
html2text = { version = "0.17", features = ["css"] }
```

- [ ] **Step 3: Edit the member crate**

In `crates/tui-do-ui/Cargo.toml`, replace

```toml
pulldown-cmark.workspace = true
```

with

```toml
comrak.workspace = true
html2text.workspace = true
```

- [ ] **Step 4: Verify it resolves and still builds**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: `Finished`. If `comrak::markdown_to_html` is later reported missing with
`default-features = false`, drop `default-features = false` and note it in the commit.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add Cargo.toml Cargo.lock crates/tui-do-ui/Cargo.toml
git commit -m "Take comrak and html2text, and drop the parser nothing imported

pulldown-cmark was scaffolded into tui-do-ui by d63357f and never used by a
single line of source. comrak renders markdown to HTML and html2text renders
HTML to wrapped, annotated lines, which is the pipeline the design settled on."
```

---

### Task 2: The router

**Files:**
- Create: `crates/tui-do-ui/src/markdown.rs`
- Modify: `crates/tui-do-ui/src/lib.rs:47` (add `pub mod markdown;`, keeping the list alphabetical — it goes between `keymap` and `modal`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub(crate) fn looks_like_html(description: &str) -> bool` — used by Task 5's
  `render`. `pub(crate)` because nothing outside this module has any business asking.

This is the one place the feature can still lose text, so it gets its own task and its own
tests. A bare `contains('<')` is **wrong**: `Use Vec<String> here` contains `<` and must go
down the *markdown* branch, because on the HTML branch `<String>` is an unknown element and
`html2text` drops it — measured, and it is exactly what `rows.rs`'s existing tests guard.

- [ ] **Step 1: Write the failing tests**

Create `crates/tui-do-ui/src/markdown.rs` containing only the test module and a stub:

```rust
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
const HTML_BLOCKS: &[&str] = &[
    "<p>", "<p ", "<h1", "<h2", "<h3", "<h4", "<h5", "<h6", "<ul", "<ol", "<li",
    "<pre", "<table", "<div", "<blockquote", "<br",
];

/// Whether this description came out of the web editor rather than a keyboard.
pub(crate) fn looks_like_html(_description: &str) -> bool {
    false
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
        assert!(looks_like_html("<pre><code>1. Enable RTSP streams</code></pre>"));
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
        assert!(!looks_like_html("# Plan: Migrate Forgejo\n\n## Overview\n\n- one\n- two"));
        assert!(!looks_like_html("| a | b |\n|---|---|\n| 1 | 2 |"));
    }
}
```

Note `<pre-release plan>` and `<pre` in the same file: `"<pre-release plan>".contains("<pre")`
is **true**, so a naive `contains` fails this test. That is the point of writing it first.

- [ ] **Step 2: Wire the module in and run the tests to watch them fail**

Add to `crates/tui-do-ui/src/lib.rs`, between `pub mod keymap;` and `pub mod modal;`:

```rust
pub mod markdown;
```

Run:

```bash
cargo test -p tui-do-ui markdown:: 2>&1 | tail -20
```

Expected: `html_from_the_web_editor_is_recognised` FAILS (the stub always returns `false`);
the other two pass vacuously.

- [ ] **Step 3: Implement the router**

Replace the stub with:

```rust
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
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p tui-do-ui markdown:: 2>&1 | tail -20
```

Expected: all three PASS. If `is_none_or` is rejected, the toolchain predates it — use
`.map_or(true, |c| …)` instead.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p tui-do-ui --all-targets 2>&1 | tail -5
git add crates/tui-do-ui/src/markdown.rs crates/tui-do-ui/src/lib.rs
git commit -m "Tell a description written in HTML from one that merely says Vec<String>

The router is the one place this feature can still lose text. contains('<') is
wrong -- six real descriptions on dev carry angle brackets in prose, and on the
HTML branch html5ever reads the bracketed word as an unknown element and drops
it. The test is a block tag, followed by a character that can actually follow a
tag name, which is what keeps '<pre-release plan>' out of the '<pre' arm."
```

---

### Task 3: The decorator

**Files:**
- Modify: `crates/tui-do-ui/src/markdown.rs`

**Interfaces:**
- Consumes: `looks_like_html` (not yet — Task 5 joins them).
- Produces: `struct Deco { theme: Theme }` implementing
  `html2text::render::TextDecorator<Annotation = ratatui::style::Style>`.

`TextDecorator::Annotation` is arbitrary, so it is a ratatui `Style` and no translation
layer is needed. Each method returns `(prefix_string, Annotation)`; `html2text` nests them
outer-first and Task 4 folds them with `Style::patch`.

- [ ] **Step 1: Write the failing test**

Add to the test module in `markdown.rs`:

```rust
    use crate::theme::{ColorDepth, Theme};
    use html2text::render::TextDecorator;
    use ratatui::style::Modifier;

    fn deco() -> Deco {
        Deco { theme: Theme::new(ColorDepth::TrueColor) }
    }

    #[test]
    fn the_decorator_answers_in_the_projects_own_theme() {
        let theme = Theme::new(ColorDepth::TrueColor);
        let mut d = deco();
        assert_eq!(d.decorate_strong_start().1, theme.text().add_modifier(Modifier::BOLD));
        assert_eq!(d.decorate_em_start().1, theme.text().add_modifier(Modifier::ITALIC));
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
```

`decorate_strong_start` and friends take `&self`; `decorate_link_start` takes `&mut self`,
which is why `d` is `mut`.

- [ ] **Step 2: Run it to watch it fail**

```bash
cargo test -p tui-do-ui markdown:: 2>&1 | tail -20
```

Expected: FAIL, `cannot find type Deco in this scope`.

- [ ] **Step 3: Implement the decorator**

Add to `markdown.rs`, above the test module:

```rust
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
        (String::new(), self.theme.text().add_modifier(Modifier::BOLD))
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
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p tui-do-ui markdown:: 2>&1 | tail -20
```

Expected: PASS. If the compiler reports missing trait methods, implement them by returning
`(String::new(), self.theme.text())` — the trait has grown between releases and the spec
pins 0.17.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p tui-do-ui --all-targets 2>&1 | tail -5
git add crates/tui-do-ui/src/markdown.rs
git commit -m "Paint markdown in the theme the rest of the interface uses

TextDecorator's Annotation is whatever the implementor says, so it is a ratatui
Style and there is no second palette. Foregrounds only, because theme.rs paints
no backgrounds, and the bullet stays the '•' plain_text used so the pane does
not change shape just because the renderer under it did."
```

---

### Task 4: `render` — the two branches joined

**Files:**
- Modify: `crates/tui-do-ui/src/markdown.rs`

**Interfaces:**
- Consumes: `looks_like_html` (Task 2), `Deco` (Task 3).
- Produces: `pub fn render(description: &str, width: u16, theme: Theme) -> Vec<Line<'static>>`
  — called by `view::preview` in Task 6.

- [ ] **Step 1: Write the failing tests**

Add to the test module. `text_of` is a helper the later tasks reuse.

```rust
    use ratatui::text::Line;

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn rendered(description: &str, width: u16) -> Vec<String> {
        text_of(&render(description, width, Theme::new(ColorDepth::TrueColor)))
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
        assert_eq!(rendered("Use Vec<String> here", 60), vec!["Use Vec<String> here"]);
        assert_eq!(rendered("a < b and b > c", 60), vec!["a < b and b > c"]);
        assert_eq!(rendered("a <b", 60), vec!["a <b"]);
        assert_eq!(rendered("<pre-release plan>", 60), vec!["<pre-release plan>"]);
        assert_eq!(
            rendered("# - CAM_<CAMERA_MAC>_NAME=fr", 60),
            vec!["# - CAM_<CAMERA_MAC>_NAME=fr"]
        );
    }

    #[test]
    fn entities_in_stored_html_decode_once() {
        // Carried across from `rows::plain_text`. `&#x27;` is what the dev instance
        // stores; `&#39;` and `&#34;` are what Go's html.EscapeString emits.
        assert_eq!(rendered("<p>Tom &amp; Jerry &lt;3</p>", 60), vec!["Tom & Jerry <3"]);
        assert_eq!(
            rendered("<p>&amp;lt; stays escaped</p>", 60),
            vec!["&lt; stays escaped"]
        );
        assert_eq!(rendered("<p>TechHut&#x27;s homelab</p>", 60), vec!["TechHut's homelab"]);
        assert_eq!(rendered("<p>say &#34;hello&#34;</p>", 60), vec!["say \"hello\""]);
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
            lines[0].spans.iter().any(|s| s.style == link && s.content.contains("ftc.gov")),
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
            spans.iter().any(|s| s.style == style && s.content.contains(text))
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
            let _ = render("some text that cannot fit", width, Theme::new(ColorDepth::TrueColor));
        }
    }
```

- [ ] **Step 2: Run them to watch them fail**

```bash
cargo test -p tui-do-ui markdown:: 2>&1 | tail -20
```

Expected: FAIL, `cannot find function render in this scope`.

- [ ] **Step 3: Implement `render`**

Add to `markdown.rs`:

```rust
use ratatui::text::{Line, Span};

/// The narrowest pane worth rendering into.
///
/// `html2text` is given a width and a one- or two-column pane has nothing useful to say;
/// asking it to wrap into that is how a renderer ends up in a loop or a panic.
const FLOOR: u16 = 4;

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
        // the 487 descriptions on dev carry their structure in single newlines, and
        // conforming here runs an address block together into one line.
        options.render.hardbreaks = true;
        // Text that looks like a tag stays text. Without this `<String>` in
        // `Use Vec<String> here` is read as raw HTML and dropped, silently.
        options.render.escape = true;
        options.extension.table = true;
        options.extension.strikethrough = true;
        options.extension.autolink = true;
        comrak::markdown_to_html(description, &options)
    };

    let Ok(lines) = html2text::config::with_decorator(Deco { theme })
        .lines_from_read(html.as_bytes(), usize::from(width.max(FLOOR)))
    else {
        return Vec::new();
    };

    lines
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
        .collect()
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p tui-do-ui markdown:: 2>&1 | tail -30
```

Expected: all PASS.

If `a_single_newline_is_a_line_break` fails, `render.hardbreaks` is not being applied —
check the option name against `comrak::Options`'s `render` field. If
`prose_containing_angle_brackets_survives_the_round_trip` fails with `Use Vec here`,
`render.escape` is not set. Those two are the load-bearing options; everything else is
cosmetic.

If `an_empty_description_renders_nothing` fails because `html2text` returned one blank
line, filter trailing lines whose spans are all whitespace before returning.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p tui-do-ui --all-targets 2>&1 | tail -5
git add crates/tui-do-ui/src/markdown.rs
git commit -m "Render a description, whichever of the three things it is

comrak for markdown and plain text, html2text for both, and the two options
that carry the whole feature: render.hardbreaks so a single newline stays a
line break, and render.escape so 'Use Vec<String> here' does not come out
'Use Vec here'. Both have a named test, because both fail silently by
deleting the user's text."
```

---

### Task 5: Heading colour, through a user-agent stylesheet

**Files:**
- Modify: `crates/tui-do-ui/src/markdown.rs`

**Interfaces:**
- Consumes: `render` (Task 4), `Deco` (Task 3).
- Produces: no new public names; `render` gains a stylesheet and `Deco` gains
  `push_colour`/`pop_colour`.

`RichAnnotation` has no heading variant and `TextDecorator` offers `header_prefix(level)`
but no annotation for it — so a heading can be prefixed and not coloured. `push_colour` *is*
on the trait, and `html2text`'s `css` feature applies a user-agent stylesheet, so the theme
becomes a few CSS rules and heading colour arrives as our own `Style`.

**If this task proves awkward, stop and take the fallback**: delete the stylesheet, keep
`header_prefix`, and headings render as `# Heading` uncoloured. That is honest, is what most
terminal renderers do, and the feature is complete without it. Do not spend a session here.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_heading_is_coloured_not_merely_prefixed() {
        let theme = Theme::new(ColorDepth::TrueColor);
        let lines = render("# Plan: Migrate Forgejo\n\nbody text", 60, theme);
        let heading = lines
            .iter()
            .find(|l| text_of(std::slice::from_ref(l))[0].contains("Migrate Forgejo"))
            .expect("no heading line");
        assert!(
            heading.spans.iter().any(|s| s.style != Style::new() && s.style != theme.text()),
            "the heading was not styled: {:?}",
            heading.spans
        );
    }
```

- [ ] **Step 2: Run it to watch it fail**

```bash
cargo test -p tui-do-ui markdown::tests::a_heading_is_coloured 2>&1 | tail -20
```

Expected: FAIL, "the heading was not styled".

- [ ] **Step 3: Add the colour hook and the stylesheet**

Add to `impl TextDecorator for Deco`:

```rust
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
```

Add above `render`:

```rust
/// The theme, as the few CSS rules `html2text` can act on.
///
/// `TextDecorator` has `header_prefix` but no header *annotation*, so this is the only
/// way a heading gets a colour. Anything not listed here is painted by the decorator.
fn agent_css(theme: Theme) -> String {
    let Some(rgb) = style_rgb(theme.accent()) else {
        return String::new();
    };
    let muted = style_rgb(theme.muted()).unwrap_or(rgb);
    format!(
        "h1,h2,h3,h4,h5,h6 {{ color: #{:02x}{:02x}{:02x} }}\n\
         blockquote {{ color: #{:02x}{:02x}{:02x} }}\n",
        rgb.0, rgb.1, rgb.2, muted.0, muted.1, muted.2
    )
}

/// The RGB of a style's foreground, when it has one that CSS can name.
fn style_rgb(style: Style) -> Option<(u8, u8, u8)> {
    match style.fg {
        Some(ratatui::style::Color::Rgb(r, g, b)) => Some((r, g, b)),
        _ => None,
    }
}
```

Change the `html2text` call in `render` to apply it:

```rust
    let config = html2text::config::with_decorator(Deco { theme });
    let config = match config.add_agent_css(&agent_css(theme)) {
        Ok(config) => config,
        // A stylesheet this module wrote failing to parse is a bug here, not bad input,
        // and it costs colour rather than correctness — render without it.
        Err(_) => html2text::config::with_decorator(Deco { theme }),
    };
    let Ok(lines) = config.lines_from_read(html.as_bytes(), usize::from(width.max(FLOOR)))
    else {
        return Vec::new();
    };
```

- [ ] **Step 4: Run the full module**

```bash
cargo test -p tui-do-ui markdown:: 2>&1 | tail -30
```

Expected: all PASS, including the tests from Task 4 — the stylesheet must not disturb them.

If `Theme::new(ColorDepth::TrueColor).accent()` does not yield an `Rgb` colour, check
`theme.rs`'s `Shade::at`; on a 256-colour or 16-colour depth it returns an indexed colour
and `agent_css` correctly returns an empty stylesheet, so test at `TrueColor` only.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p tui-do-ui --all-targets 2>&1 | tail -5
git add crates/tui-do-ui/src/markdown.rs
git commit -m "Colour headings by handing html2text the theme as a stylesheet

TextDecorator has header_prefix and no header annotation, so a heading can be
prefixed and not coloured. push_colour is on the trait and the css feature
applies a user-agent sheet, so theme.rs becomes four CSS rules and the colour
comes back as our own Style. A sheet that will not parse costs colour, not
correctness."
```

---

### Task 6: Draw it in the preview pane

**Files:**
- Modify: `crates/tui-do-ui/src/view.rs:448-467` (the description block inside `preview`)
- Modify: `crates/tui-do-ui/src/view.rs:385` (delete `MAX_PARAGRAPH_LINES`)

**Interfaces:**
- Consumes: `markdown::render` (Task 4).
- Produces: nothing new.

The existing block computes `budget` — `preview_scroll + inner.height` — and stops laying
out once it has enough lines to fill the pane. That discipline stays: it is why a 17 KB
description does not cost a full layout every frame.

**Two details the old code carries that are easy to drop.** Every line in this pane is
inset by one space — `format!(" {line}")`, and `width` is already
`inner.width.saturating_sub(1)` to pay for it — so the rendered lines need the same inset
or the description will sit a column left of the title and the fields above it. And
`MAX_PARAGRAPH_LINES` (`view.rs:385`) exists only for the `rows::wrap` call being removed;
leaving it behind is a clippy `dead_code` failure under `-D warnings`.

- [ ] **Step 1: Read the current block**

```bash
sed -n '445,470p' crates/tui-do-ui/src/view.rs
```

Confirm it matches what Step 2 replaces. If it has moved, find it with
`grep -n "plain_text" crates/tui-do-ui/src/view.rs`.

- [ ] **Step 2: Replace it**

Replace from the `// Descriptions are HTML from the web editor.` comment through the end of
that `if !description.is_empty() { … }` block with:

```rust
    // Rendered rather than stripped: `markdown::render` answers with styled, wrapped
    // lines whether the description is Markdown, plain text, or the HTML the web editor
    // stores. It runs in process, so this stays a pure call from `view`.
    let rendered = markdown::render(&task.description, width, theme);
    if !rendered.is_empty() {
        lines.push(Line::default());
        // Only what can be seen is laid out. Rendering is cheap -- a median description
        // is 118 bytes -- but pushing every line of a 17 KB one into a vector to draw the
        // dozen that fit is the same waste the task list already avoids.
        let budget = usize::from(model.list.preview_scroll) + usize::from(inner.height);
        for line in rendered {
            if lines.len() >= budget {
                break;
            }
            // The pane insets everything by one column, which is why `width` above is
            // `inner.width - 1`. The rendered lines pay it the same way the title and
            // the fields do.
            let mut spans = vec![Span::raw(" ")];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
    }
```

Then delete `MAX_PARAGRAPH_LINES` and its doc comment at `view.rs:384-385`:

```rust
/// The most lines one paragraph of a description may take before it is ellipsised.
const MAX_PARAGRAPH_LINES: u16 = 40;
```

Add `use crate::markdown;` to the imports at the top of `view.rs`, after
`use crate::keymap::…` — the list is alphabetical. `Span` and `Line` are already imported.

- [ ] **Step 3: Build and run the whole suite**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -20
```

Expected: builds; every test passes. `rows::plain_text` is still present and still tested
at this point — Task 7 removes it.

- [ ] **Step 4: Look at it**

```bash
cargo build --release --workspace
```

Then run `tui-do`, select a task with a description, and check the preview pane. Section F
of `md/MANUAL-CHECKS2.md` is the model for how these are recorded. Confirm:
- a plain-text description keeps its line breaks;
- an HTML description shows structure, not tags;
- resizing the terminal re-wraps the text;
- a description with a heading shows it coloured;
- the description is inset one column, lining up with the title and the fields above it.

These are real tasks on dev rather than invented ones. To find them:

```bash
sqlite3 ~/.local/share/tui-do/tui-do.db \
  "select id, substr(title,1,60) from tasks where description like '%<p>%' limit 3;"
sqlite3 ~/.local/share/tui-do/tui-do.db \
  "select id, substr(title,1,60) from tasks
   where description not like '%<%>%' and description like '%#%' limit 3;"
```

`~/.local/bin/tui-do` is a symlink to `target/release/tui-do`, so the release build is what
this exercises. A debug build proves the tests pass and changes nothing the user sees.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets 2>&1 | tail -5
git add crates/tui-do-ui/src/view.rs
git commit -m "Draw the description instead of stripping its tags

The preview pane renders through markdown::render now. The budget stays: only
what can be seen is laid out, which is why a 17 KB description does not cost a
full layout every frame."
```

---

### Task 7: Delete `plain_text`

**Files:**
- Modify: `crates/tui-do-ui/src/rows.rs` — delete lines 602–924 (`ELEMENTS`, `BLOCK`,
  `OPAQUE`, `plain_text`, `Tag`, `apply_tag`, `push_text`, `comment_end`, `read_tag`,
  `skip_element`, `read_entity`) and their tests.

**Interfaces:**
- Consumes: nothing.
- Produces: nothing. `display_width`, `truncate`, `wrap` and `relative_date` are untouched —
  they serve the task list.

Done as its own task, after Task 6, so that the commit that removes the old renderer is
separate from the one that adds the new one and either can be reverted alone.

- [ ] **Step 1: Confirm nothing calls it**

```bash
grep -rn "plain_text" crates/ --include=*.rs | grep -v "^crates/tui-do-ui/src/rows.rs"
```

Expected: no output. If `view.rs` still appears, Task 6 is incomplete — go back.

- [ ] **Step 2: Delete the functions and their tests**

Remove the block from `const ELEMENTS: &[&str] = &[` through the end of `read_entity`, and
in the test module remove `html_from_the_web_editor_reads_as_text`,
`prose_that_contains_angle_brackets_is_not_eaten`,
`something_that_merely_starts_like_a_tag_is_still_text` and
`entities_decode_once_including_the_numeric_forms`.

Every assertion in those four tests already lives in `markdown.rs` — Task 2 took the routing
cases and Task 4 took the entity and round-trip cases. Verify that before deleting:

```bash
grep -n "Vec<String>\|CAM_<CAMERA_MAC>\|pre-release plan\|&#x27;\|&#8212;" crates/tui-do-ui/src/markdown.rs
```

Expected: every one of those strings appears. If any is missing, add it to `markdown.rs`
first.

- [ ] **Step 3: Build and test**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -20
cargo clippy --workspace --all-targets 2>&1 | tail -10
```

Expected: builds, all tests pass, clippy clean with no unused-import or dead-code warnings.
Remove any import that only `plain_text` needed.

- [ ] **Step 4: Confirm the purity guard still holds**

```bash
cargo test -p tui-do-ui a_pure_ui_names_no_io 2>&1 | tail -5
```

Expected: PASS. Neither `comrak` nor `html2text` names a store or awaits anything, but the
guard is cheap to run and this is the commit that would notice.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/tui-do-ui/src/rows.rs
git commit -m "Remove the tag stripper, now that something renders

plain_text called itself a stopgap in its own doc comment and it was right. What
it knew that was worth keeping -- that prose says Vec<String> and a < b, and
that '<pre-release plan>' is not a <pre> tag -- moved to markdown.rs's router
and round-trip tests before this deletion, not after."
```

---

### Task 8: Bring the documentation up to date

**Files:**
- Modify: `PLAN.md:256-263` (Phase 5's markdown bullet)
- Modify: `CLAUDE.md` (a new entry under "Wire-format facts the spec does not tell you")

**Interfaces:** none.

- [ ] **Step 1: Rewrite Phase 5's markdown bullet in `PLAN.md`**

Replace the bullet beginning `- **Markdown task descriptions via `glow`.**` with:

```markdown
- **Task descriptions, rendered.** A description is Markdown, plain text, or the HTML the web
  editor stores, and the API does not say which. `comrak` renders the first two to HTML, the
  third is HTML already, and `html2text` turns both into wrapped lines annotated with styles
  from `theme.rs`. In process, so `update` gains no `Effect` and the model gains no cache.
  **`glow` was specified here and was measured and rejected on 2026-08-30** — it flattens an
  HTML description to a single run, pads every line to a fixed width with styled spaces,
  emits OSC 8 that ratatui cannot represent, and paints in its own palette. None of that is
  particular to `glow`, so `markdown_renderer: glow | builtin | auto` and `doctor`'s
  markdown line are dropped with it: there is no external binary left to choose between.
  Design and measurements in `md/2026-08-30-markdown-descriptions-design.md`.
```

- [ ] **Step 2: Add the wire-format entry to `CLAUDE.md`**

Add after the "Assignees travel in the task body; labels do not" section:

```markdown
**A description is Markdown, plain text, or HTML, and nothing on the wire says which.**
Measured on dev 2026-08-30 across 487 non-empty descriptions: 367 carry no tag at all, 108
are TipTap HTML from the web editor, and the rest are mixed. A task written through the API
keeps what was sent; a task touched in the web editor comes back as HTML. Both persist, so
every reader meets both.

What tells them apart is a **recognised HTML block tag**, not a `<`: real descriptions say
`Use Vec<String> here` and `# - CAM_<CAMERA_MAC>_NAME=fr`, and routing those to an HTML
parser deletes the bracketed word. `markdown::looks_like_html` is the one place that
decision is taken.

Two `comrak` options in `markdown::render` are load-bearing and both fail *silently, by
deleting text*. `render.hardbreaks` keeps a single newline as a line break — CommonMark
joins consecutive lines, which runs an address block together, and roughly 350 of those 367
descriptions carry their structure in single newlines. `render.escape` keeps text that looks
like a tag as text — without it `<String>` is read as raw HTML and dropped, and the line
reads `Use Vec here`. Each has a named regression test.
```

- [ ] **Step 3: Check the claims still hold**

```bash
grep -n "markdown_renderer\|glow" PLAN.md CLAUDE.md README.md
```

Expected: no stale reference promising a `markdown_renderer` config key or a `doctor`
markdown line. Fix any that remain.

- [ ] **Step 4: Full verification**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -20
cargo build --release --workspace 2>&1 | tail -3
```

Expected: fmt clean, clippy clean, all tests pass, release builds.

- [ ] **Step 5: Commit**

```bash
git add PLAN.md CLAUDE.md
git commit -m "Say what a description is, and stop promising a glow renderer

Phase 5 specified glow with a markdown_renderer config key and a doctor line
reporting which renderer was active. There is no external binary left to choose
between, so all three go. The wire-format note records what cost the measuring:
a description is markdown, text, or HTML with nothing on the wire to say which,
and the two comrak options that keep it intact both fail by deleting text."
```

---

## Self-review

**Spec coverage.** Every section of the design maps to a task: the router is Task 2 (design
decision 3), the decorator Task 3 (decision 5), `hardbreaks` and `escape` Task 4 (decisions
1 and 2), the stylesheet Task 5 (decision 5's heading half), wrapping is inherent to Task 4
(decision 4), `extension.table` is in Task 4's options (decision 6), no cache is the absence
of state anywhere (decision 7), the dependency changes are Task 1, `plain_text`'s deletion
is Task 7, and the documentation is Task 8. The design's testing list is distributed across
Tasks 2, 4 and 5; every bullet in it appears in a named test.

**Naming consistency.** `looks_like_html`, `Deco`, `render`, `agent_css`, `style_rgb`,
`HTML_BLOCKS`, `FLOOR` — each defined once and referenced with the same name and signature
throughout. `render(description: &str, width: u16, theme: Theme) -> Vec<Line<'static>>` is
identical in Tasks 4, 5 and 6. `text_of` and `rendered` are defined in Task 4 and reused in
Task 5.

**Two things deliberately left as judgement calls**, both flagged in place: Task 1 Step 4
says what to do if `default-features = false` hides `markdown_to_html`, and Task 5 says to
take the uncoloured-heading fallback rather than spend a session on the stylesheet.

**One behaviour change from the old renderer**, tested and documented rather than
discovered: `<http://Www.ftc.gov>` is a CommonMark autolink, so it renders as
`http://Www.ftc.gov` — styled, brackets consumed — where `plain_text` kept the brackets.
`an_autolink_loses_its_brackets_and_gains_a_style` in Task 4 pins it.
