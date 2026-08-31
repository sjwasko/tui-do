# tui-do — rendering task descriptions: the design, before any code

Written 2026-08-30, resuming Phase 5. `PLAN.md` specifies descriptions rendered through
`glow` with a `pulldown-cmark` fallback, configurable as `markdown_renderer:
glow | builtin | auto`, with `tui-do doctor` reporting which is active.

**This document ends that plan and replaces it.** Everything below was measured on this
workstation against the real store (487 non-empty descriptions), against `glow` 3.0.0, and
against the candidate libraries themselves. Three of the four items `PLAN.md` names stop
existing: the subprocess, the config key, and `doctor`'s markdown line.

**Revised the same day, before any code.** The first version of this document routed
HTML → markdown → spans, and named its own weak point: converting *to* markdown means the
output is parsed again, so text that was inert as HTML becomes syntax on the second pass,
and escaping that correctly is the part a hand-rolled walker does not do. The pipeline is
now inverted — markdown → HTML → spans — which does not have that failure mode at all,
because HTML never faces a second parse. The evidence against `glow` is unchanged and the
sections recording it stand.

## What the data actually is

`PLAN.md` says "Vikunja stores descriptions as HTML/Markdown". Both, in one column, and
the split is not the one the phrase implies:

| shape | count |
|---|---|
| no tag at all | 367 |
| HTML from the web editor (`<p>`, `<h1>`…`<h3>`) | 108 |
| markdown wrapped in `<pre>`, entities escaped | 1 |
| contains `<table>` | 3 |

A task written through the API keeps whatever text was sent. A task touched in the web
editor comes back as TipTap HTML. Both persist indefinitely, so any renderer meets both.

**The 367 tag-free descriptions are mostly not markdown either.** Of them, 6 have a line
starting with `#`, 2 have a fenced block, 3 have a `[]()` link, 7 have a pipe-table row
and 2 have `**`. The rest — roughly 350 — are plain text whose structure is carried
entirely by single newlines. That fact drives the `hardbreaks` decision.

Sizes bound the cost of everything below: median 118 bytes, tenth-largest 5,372, largest
16,987. And the feature is smaller than it sounds: 487 of 3,876 tasks have a description
at all, so this renders a field that is empty seven times out of eight.

## What `glow` does, measured

**It does not pass HTML through. It destroys it.** Given

```html
<h1>Hermes Report</h1><p><strong>Date:</strong> 2026-07-03</p><ul><li>one</li><li>two</li></ul>
```

`glow -s dark -w 56` answers with one line:

```
Hermes ReportDate: 2026-07-03onetwo
```

Tags swallowed, text concatenated, no breaks, words run together. For 108 descriptions
that is worse than the `plain_text` stopgap it was meant to replace, which at least breaks
on block elements. This is not a `glow` bug: CommonMark specifies that raw HTML is passed
through to the output, which is right when the output is HTML and undefined when it is a
terminal. Every terminal markdown renderer invents an answer here and none of them
interpret the HTML, because that is a different program.

**Its ANSI is hostile to ratatui**, in four ways:

| what | consequence |
|---|---|
| every line padded to `-w` with *styled* spaces | a blank line at width 60 is 56 separate spans |
| one SGR pair per token | `Hermes Report` is four spans, not one |
| OSC 8 hyperlinks — `ESC]8;id=…;https://…ESC\` | ratatui cannot represent them; passed through raw they corrupt the frame |
| its own palette — the heading came back on `48;5;63` | `theme.rs` is 407 lines of deliberate colour that `glow` overrides |

There is no `glow` mode that is both styled and well-behaved: `-s notty` and `-s ascii`
drop the SGR spam but still pad, still emit OSC 8, and give up all styling.

**A better binary would not help.** Rendering to a fixed width, in its own palette, as
terminal escapes rather than spans are properties of the category, not of `glow`. `w3m
-dump` was tried on the same HTML and renders it correctly — headings, lists, tables,
links — but emits unstyled text and is a second external binary. The way out is not a
better viewer; it is doing both jobs in process.

## The shape

```
task.description
  ├─ contains a known HTML block tag?  ──────────────────┐
  └─ otherwise ─→ comrak ─→ HTML ────────────────────────┤
                  hardbreaks: true                       │
                  escape: true                           ▼
                  extension.table: true          html2text + Deco
                  extension.autolink: true      (Annotation = Style)
                  extension.strikethrough: true
                                                         │
                                          Vec<TaggedLine<Vec<Style>>>
                                                         │
                                                 Vec<Line<'static>>
```

One styling layer, one wrap engine, two entry points. A new module,
`crates/tui-do-ui/src/markdown.rs`, pure and synchronous, called from `view`.

**Rule 1 is not merely respected here, it is unengaged.** No subprocess means no `Effect`,
no `Msg`, no model state and no cache. `PLAN.md` specified a cache keyed per task and
invalidated on width because it assumed a subprocess, and a fork-exec at 5–20ms genuinely
needs one; parsing a median 118-byte string does not. The loop draws once per message with
a one-second `Tick` floor.

**Revised 2026-08-31, because the data outgrew the measurement.** The sizes above — median
118 bytes, largest 16,987 — were the whole store on 2026-08-30. A session transcript pasted
into a task made one of **131,204 bytes**, nine times the previous maximum, and it renders in
**7.7 ms**. That is still inside a frame budget and the no-cache decision stands, but the
claim that this is "one parse per second of a string smaller than this paragraph" is no
longer true, and the margin is thinner than it reads. If a description an order of magnitude
larger again appears, the preview pane will feel sticky while that task is selected, and a
cache keyed by task id and width becomes worth its complexity.

`a_pure_ui_names_no_io` keeps passing. It greps this crate's source for `Store`, `.await`,
`spawn_blocking` and three crate names; neither library is any of those and neither
touches I/O.

## The decisions

**1. `render.hardbreaks` is on, so a soft line break is a line break.** CommonMark joins
consecutive lines into one paragraph. Against this data that is destructive: a real
description reading

```
Manatee County:
# to call:  941-749-1800
Bradenton - old historic courthouse
1115 Manatee Ave West
830a-430p
```

renders, through a conforming renderer, as `Bradenton - old historic courthouse 1115
Manatee Ave West 830a-430p` — an address block collapsed into one run. Measured through
`tui-markdown` 0.3.9, which does exactly this, and which also collapsed a Clerk of Court
address and a letter checklist in the same description. With `hardbreaks` the same
description renders with every line intact, verified.

This was a hand-rolled deviation from CommonMark in the first draft of this design,
needing a defensive comment so nobody "fixed" it back. It is now a supported library
option, which is the single best argument for the inverted pipeline.

It also removes a decision the design would otherwise have to take. **Nothing has to guess
whether a tag-free description is markdown or plain text**, because with this rule both
render the same way: every line stays a line, and the handful of descriptions carrying a
real `#` or fence get that construct rendered as a bonus.

**2. `render.escape` is on, and it is load-bearing.** Without it, comrak reads `<String>`
in `Use Vec<String> here` as raw inline HTML and — with `unsafe_` off — emits
`<!-- raw HTML omitted -->`, which html2text then drops entirely. Measured: the line came
out as `Use Vec here`. That is exactly the regression `rows.rs:1149` guards against.

With `escape` on, all six of the existing not-a-tag assertions survive the full round
trip, verified:

| input | after the pipeline |
|---|---|
| `Use Vec<String> here` | `Use Vec<String> here` |
| `a < b and b > c` | `a < b and b > c` |
| `Tom &amp; Jerry &lt;3` | `Tom & Jerry <3` |
| `# - CAM_<CAMERA_MAC>_NAME=fr` | `# - CAM_<CAMERA_MAC>_NAME=fr` |
| `<pre-release plan>` | `<pre-release plan>` |
| `a <b` | `a <b` |

and `**bold**`, `*em*`, `` `code` `` and `[link](url)` still produce `<strong>`, `<em>`,
`<code>` and `<a>`. `escape` only ever applies on the markdown branch; genuine HTML does
not pass through comrak at all.

**3. The router is the one place this can still go wrong, and it is where `rows.rs`'s tag
knowledge lives on.** Something must choose "HTML straight in" against "markdown through
comrak", and misrouting is what broke `Vec<String>` above: sent down the HTML branch,
`<String>` is an unknown element and html2text drops it. A bare `contains('<')` is
therefore *wrong*, and it is what the crude SQL in the table above used — which is why
`Use Vec<String> here` counts in the 120 rather than the 367.

This section undersold the router from the start and then went stale three times more as
real descriptions found holes in it; `markdown.rs`'s own doc comment on `looks_like_html`
is now the authoritative account, this is only a summary. The rule that shipped: the
trimmed description either *starts with* a recognised HTML block tag — `<p`, `<h1`…`<h6`,
`<ul`, `<ol`, `<pre`, `<table`, `<div`, `<blockquote` — or contains, anywhere in the body,
both an opening tag and its own matching closing tag for the same element (`<div` paired
with `</div>`, `<p` paired with `</p>`, and so on).

Three earlier rounds landed on weaker rules, each closing one hole and opening or leaving
another: unanchored ("a block tag mentioned anywhere"), a bare mention of a tag in prose
(`"Wrap it in a <div>"`) misrouted to HTML and lost the bracketed word to `html5ever`.
Anchoring the opening half to "starts with a block tag" fixed that but left the
closing-tag check unanchored, so a sentence merely *describing* a close (`"close the
</div> tag"`) still misrouted. Anchoring *both* halves to "starts with `<`" fixed that in
turn, but broke ids 49, 369 and 420 on dev — genuine TipTap output shaped as a leading
sentence of bare text followed by real `<div><br></div>` blocks (the shape pasting from
Google Keep into TipTap produces), none of which opens on `<`.

The pairing rule now in place drops the "starts with `<`" anchor and asks a stronger
question instead: a genuine TipTap document always closes what it opens, wherever in the
string that falls, and prose mentioning markup supplies at most one half at a time. That
one fact — the pair, not the position — separates every case the three earlier rounds
were fighting over, including the `<img>`-first shape (store ids 27, 116) that opens on a
tag outside the block list but still carries a bare `<p></p>` pair. Validated against the
487 real descriptions on dev: 112 classify as HTML, up from 108 under the previous rule —
ids 49, 369 and 420 moved back to HTML as intended, and one previously-unexamined row (id
1842, free-form notes with a genuine `<p>…</p>` fragment pasted mid-text) moved with them,
consistent with the rule as stated.

**Known residual, accepted rather than chased:** a sentence naming *both* halves of the
same element — `"use <div> and close with </div>"` — still misroutes to HTML and loses
both words. Nothing short of parsing the sentence separates that from a real TipTap paste
of an empty `<div></div>`, and three rounds of narrowing the discriminator by string shape
alone is enough evidence that this is where that approach stops paying off. `rows.rs`
originally had the block-tag list as `BLOCK`, and its tests were carried across as router
tests. **`plain_text` is not extended into a converter, as the first draft proposed — it
is deleted, and its knowledge becomes the router.**

`plain_text` also carried an `OPAQUE` list so that `<script>` and `<style>` bodies —
code and CSS, not prose — were never shown. That list did not need porting: `html5ever`
strips both elements' bodies on its own, and `html2text` (which is built on it) inherits
that for free. Measured: a description containing `<script>alert('x')</script>` renders
with no trace of the script, with no help from this module.

**4. Wrapping is html2text's, and the widths cannot disagree with ratatui's.**
`lines_from_read(html, width)` returns lines already wrapped. `CLAUDE.md` is emphatic that
a second width implementation is a bug waiting to happen, so this was checked rather than
assumed: `html2text` measures with `unicode-width`, and in this workspace it **unifies at
0.2.2** — the same crate version `ratatui-core` measures with. There is one wrap engine
and one width table. Re-render on resize, no cache to invalidate.

**5. Styling arrives through the decorator, and headings through a stylesheet.**
`TextDecorator::Annotation` is arbitrary, so it is `ratatui::style::Style`; each method
returns `(prefix, Style)`, annotations nest outer-first, and folding them with
`Style::patch` gives one `Span` per styled run. Verified: `bold`→BOLD, `em`→ITALIC,
`code`→yellow, `link`→cyan+underlined, with tight spans rather than `glow`'s
one-per-token.

Headings are the exception. `RichAnnotation` has no heading variant and the trait offers
`header_prefix(level) -> String` but no annotation, so a heading can be prefixed and not
styled. `push_colour(&mut self, Colour) -> Option<Self::Annotation>` *is* on the trait,
and html2text's optional `css` feature applies a user-agent stylesheet — so `theme.rs`
emits a few rules (`h1`…`h6`, `blockquote`) and heading colour arrives as our own `Style`.
This costs the `css` feature and `nom`. If it proves awkward the fallback is a `#` prefix
with no colour, which is honest and is what most terminal renderers do.

**That fallback also happens on a non-TrueColor terminal, and it is deliberate rather than
accidental.** `agent_css` builds its stylesheet from `style_rgb`, which matches only
`Color::Rgb`; `theme.rs`'s `Shade::at` answers `Color::Blue` at `ColorDepth::Ansi16` and
`Color::Indexed` at `Ansi256`, neither of which CSS can name. So on those two depths
`style_rgb` returns `None`, `agent_css` returns an empty stylesheet, and a heading keeps
`header_prefix`'s `#` with no colour — the same fallback named above, reached by depth
rather than by a `css` failure. `style_rgb` carries a comment saying so.

**`html2text` 0.17.1's CSS parser silently drops a block's final declaration if it is
missing its trailing `;`.** `add_agent_css` returns `Ok` either way, so a rule written
without the semicolon on its last line is a no-op, not an error, and the symptom is a
heading that simply comes out uncoloured. Cost real debugging time to trace back to a
missing character. Every rule in `theme.rs`'s stylesheet carries a trailing `;`, including
the last one in each block.

**6. Tables are html2text's, with `extension.table` on.** comrak does not parse GFM tables
by default and the pipes came through literally until it was enabled. 10 of 487
descriptions have a table and the pane is about 40 columns. Measured rather than guessed:
a table renders as a proper boxed grid at both 60 and 40 columns, so the width the preview
pane actually has is not a concern here.

Two more extensions are on. `extension.strikethrough` turns `~~struck~~` into `<del>`,
which is benign — nothing in the corpus depends on a bare `~~` staying text.
`extension.autolink` turns a bare `http://…`, with no `<>` or `[]()` around it, into a
styled link — measured: `see http://example.com/path` gets a styled span only with the
extension on. It is *not* what `an_autolink_loses_its_brackets_and_gains_a_style` tests;
that input, `<http://Www.ftc.gov>`, is a CommonMark autolink and needs no extension, and
the test still passes with `extension.autolink` off. No test currently depends on it, but
it is the same "literal syntax should render, not stay literal" reasoning behind `escape`
and `strikethrough`, and URL opening later in Phase 5 wants exactly this for a bare URL a
user typed without brackets.

**7. No cache**, from the sizes above.

## What it costs, measured

Against this workspace rather than an empty project:

| | today | after | change |
|---|---|---|---|
| crates compiled | 239 | 264 | **+25** |
| release binary | 11.9 MB | 13.5 MB | **+1.6 MB, +13.4%** |

Third-party source taken on, excluding each library's bundled test suite: comrak ~35,600
lines, html2text ~9,600, html5ever ~7,700 — about 53,000, against tui-do's own 36,800.
That is more code than this project contains, and it was accepted deliberately: it is
Servo's HTML parser and a reference-grade CommonMark implementation, and the alternative
is not writing 53,000 lines but writing 300 that handle the common cases and quietly
mangle the rest — which is what `plain_text`'s own doc comment admits it does today.

## Alternatives rejected, and why

**`glow`** — the whole of "What `glow` does" above.

**`tui-markdown` 0.3.9** — better than expected: a configurable `StyleSheet`, a real table
renderer, inline HTML shown dim rather than swallowed, and `default-features = false`
sheds `syntect`. Rejected on two counts verified in its source: `from_str` takes **no
width**, so it does not wrap (a real description "at width 40" produced a 700-character
line); and `renderer/mod.rs:382` is `fn soft_break(&mut self) { … push_span(Span::raw(" "))
}` with `Options` exposing only `image_fallback` and `code_theme`, so decision 1 is
unreachable without forking it.

**Walker → markdown → `pulldown-cmark` → spans** — this document's own first draft. It
needed hand-rolled markdown escaping (decision 2's hazard, in the direction where it is
hard), a hand-rolled wrap engine, and the soft-break deviation. It cost zero new crates,
which is its only advantage.

**`htmd`** — HTML → markdown over html5ever. Moot once the pipeline inverted; nothing
converts to markdown any more.

**`pulldown-cmark` is now unused and should be removed.** It was declared in
`crates/tui-do-ui/Cargo.toml` by the first scaffold commit, `d63357f`, in anticipation of
exactly this feature, and no source file has ever imported it. comrak replaces it.

## What changes

| file | change |
|---|---|
| `crates/tui-do-ui/src/markdown.rs` | new: the router, the `Deco` decorator, HTML → `Vec<Line<'static>>` |
| `crates/tui-do-ui/src/rows.rs` | `plain_text` deleted; its `BLOCK` list and its six not-a-tag tests become the router's |
| `crates/tui-do-ui/src/view.rs` | `preview` renders lines; the `preview_scroll` + `inner.height` budget survives |
| `crates/tui-do-ui/Cargo.toml` | `comrak`, `html2text`; `pulldown-cmark` removed |
| `Cargo.toml` | the same, in `[workspace.dependencies]` |
| `PLAN.md` | Phase 5's markdown bullet rewritten; `markdown_renderer` and `doctor`'s markdown line removed |
| `CLAUDE.md` | a wire-format entry: a description is HTML *or* text, which the spec does not say and which nothing but the block-tag test can tell apart |

## What this is not

Comments, relations and URL opening are Phase 5 and are not this. `tui-do doctor` may
still be worth building for other reasons — it has nothing to report about markdown once
there is no external binary to choose between. The `MarkdownRenderer` trait `PLAN.md`
specifies is not built: URL opening, later in this phase, is the genuine first user of the
platform trait the macOS port depends on, and is a better home for that seam.

## Testing

`tui-do-ui` is pure, so all of this is unit-testable in place and none of it belongs in
`tui-do-smoke` — there is no seam here, no store call and no message.

- **The router**, with the six not-a-tag assertions carried across from `plain_text`, plus
  one per block tag it must recognise.
- **A named regression test for `hardbreaks`**, using the Manatee County description,
  asserting the address lines do not collapse.
- **A named regression test for `escape`**, asserting `Use Vec<String> here` survives —
  because turning `escape` off is a plausible future "cleanup" and it fails silently by
  deleting text.
- One test per construct — emphasis, strong, code, link, list, blockquote, heading,
  preformatted — asserting the `Style` on the span, not just the text.
- Real HTML shapes from the store: the `<pre><code>` one and the
  `<p><a target="_blank" rel="noopener">` one.
- Wrapping at a narrow width, because the preview pane is narrow and it is the case the
  old code never had to handle.
- An empty description, and a description that is only whitespace.

## What is not covered

`markdown::render` returns an empty `Vec` both when a description is genuinely empty and
when the pipeline fails — observed with deeply nested lists at small widths, where
`html2text` reports the pane too narrow to lay the list out (`TooNarrow`). That failure is
not partial: it fails the whole document, not just the offending list, so a description
whose first paragraph is ordinary prose and which only *later* contains a deeply nested
list renders completely blank, not blank-from-that-point-on. Measured threshold: at 30
columns, the narrowest a real pane gets, a list nested 14 levels deep blanks the entire
description. `view::preview` has no way to tell a failed render apart from a genuinely
empty one, so a task with real content can show a blank description area with nothing on
screen to say why.

This is unlikely at realistic pane widths — it took deliberately nested input to trigger —
and was accepted rather than fixed: distinguishing the two cases means `render` returning a
`Result` and `preview` rendering an error state, which is more machinery than a failure
mode this narrow has earned. Recorded here so it is found by reading rather than by a user
reporting a task that looks empty and is not.

**Image-only descriptions preview as blank.** Store ids 27 and 116 are
`<img …><p></p>` — an image with no other content — and render to nothing. Measured why:
`decorate_image` does run, but only when the `<img>` carries a non-empty `alt`; with
`alt=""` or no `alt` attribute at all, `html2text` emits no text for the image and no call
reaches the decorator. TipTap's own markup is `data-src`, `src="#"`, `id` — never `alt` —
so this is not a narrow miss, it is every image TipTap has ever produced landing on the
no-text path. This is not a regression — `rows::plain_text` showed nothing for these two
either — and it is deliberately not fixed here: deciding what an image should look like in
a terminal (a placeholder, a generic `[image]`, something else) is its own piece of work,
not a side effect of this one.
