# tui-do — rendering task descriptions: the design, before any code

Written 2026-08-30, resuming Phase 5. `PLAN.md` specifies descriptions rendered through
`glow` with a `pulldown-cmark` fallback, configurable as `markdown_renderer:
glow | builtin | auto`, with `tui-do doctor` reporting which is active.

**This document ends that plan and replaces it.** Everything below was measured on this
workstation against the real store (487 non-empty descriptions) and against `glow` 3.0.0,
and the measurements say `glow` is not the good path being degraded from — it is the
degraded path. Three of the four items `PLAN.md` names stop existing: the subprocess, the
config key, and `doctor`'s markdown line.

The one decision here that is expensive to take later is **what a soft line break means**,
because it is the difference between a description that reads and one that has been run
together. It is taken in "The five decisions", and it is deliberately not CommonMark.

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
entirely by single newlines. That fact drives decision 1.

Sizes bound the cost of everything below: median 118 bytes, tenth-largest 5,372, largest
16,987.

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
  ├─ contains a known HTML block tag?  ──→  html → markdown
  └─ otherwise, use as-is  ─────────────────────┐
                                                ▼
                                      pulldown-cmark events
                                                ▼
                          render(events, width, theme) → Vec<Line<'static>>
```

A new module, `crates/tui-do-ui/src/markdown.rs`, pure and synchronous, called from
`view`. The HTML half grows out of the tag walker already in `rows.rs`.

**Rule 1 is not merely respected here, it is unengaged.** No subprocess means no `Effect`,
no `Msg`, no model state and no cache. `PLAN.md` specified a cache keyed per task and
invalidated on width because it assumed a subprocess, and a fork-exec at 5–20ms genuinely
needs one; parsing a median 118-byte string does not. The loop draws once per message with
a one-second `Tick` floor, so the idle cost is one parse per second of a string smaller
than this paragraph.

`a_pure_ui_names_no_io` keeps passing. It greps this crate's source for `Store`, `.await`,
`spawn_blocking` and three crate names; `pulldown-cmark` is none of those and touches no
I/O.

## The five decisions

**1. A soft break is a line break, not a space.** CommonMark joins consecutive lines into
one paragraph. Against this data that is destructive: a real description reading

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
address and a letter checklist in the same description.

So `Event::SoftBreak` emits a line break. This is a deliberate departure from the spec and
must carry a comment saying so, or someone will later fix it back and quietly ruin 350
descriptions.

It also removes a decision the design would otherwise have to take. **Nothing has to guess
whether a tag-free description is markdown or plain text**, because with this rule both
render the same way: every line stays a line, and the handful of descriptions carrying a
real `#` or fence get that construct rendered as a bonus. The only branch in the pipeline
is the HTML one, which is a different and answerable question.

The residual risk is incidental markdown in plain text — the 6 descriptions with a `#`
line, the 2 with `**`. Rendering `# to call: 941-749-1800` as a styled heading is odd
rather than destructive, and is accepted.

**2. Wrapping uses `rows::wrap` and `rows::display_width`.** Not a new implementation.
The workspace pins `unicode-width` to the version ratatui measures with, precisely so a
width this crate computes and a width ratatui draws cannot disagree; a second wrapper
would reintroduce that gap. It also means the render reflows on resize, which no external
binary's output can.

**3. Styling resolves through `theme.rs`.** Headings, emphasis, code, links and
blockquotes take their colour from the existing theme, not a second palette.

**4. Tables render as aligned rows, not box-drawn.** 10 of 487 descriptions contain a
table and the preview pane is about 40 columns; a box-drawn table does not fit in it. This
gives up `tui-markdown`'s 1,011-line table renderer knowingly.

**5. No cache.** From the sizes above.

## Why not `tui-markdown`

It was evaluated rather than dismissed, and it is better than expected: a configurable
`StyleSheet` (`heading(level)`, `code()`, `link()`, `blockquote()`), a real table
renderer, and inline-HTML handling that shows unknown tags dim instead of swallowing them.
With `default-features = false` it sheds `syntect` and `ansi-to-tui` and costs little. Its
`ratatui-core 0.1` matches tui-do's ratatui 0.30.

It is rejected on two counts, both verified in its source:

- **It does not wrap.** `from_str` takes no width. Rendering a real description "at width
  40" produced a 700-character line for a URL and a 67-column line for a paragraph.
- **Its soft break is not configurable.** `renderer/mod.rs:382` is
  `fn soft_break(&mut self) { … self.push_span(Span::raw(" ")) }`, and `Options` exposes
  only `image_fallback` and `code_theme`.

Two of the three things this design needs are unavailable without forking it, and the
third — theming — is the cheapest to do directly.

## Why not `htmd` for the HTML half

`htmd` is a turndown-inspired HTML→markdown converter over `html5ever`, and on the sample
above it did well: `<pre><code>` became a fence and `&amp;` decoded correctly. It costs 32
crates — `html5ever`, `markup5ever`, `xml5ever`, `string_cache`, `phf` and its codegen,
`tendril`, `parking_lot`, `serde` — against `pulldown-cmark`'s 7, several of which the
workspace already has. That weighs against GA bar item 2, a single static binary.

The decisive argument is the other way round, though. The source is TipTap, a constrained
generator with a small predictable tag set, not the open web — and `rows.rs`'s walker is
already tested against this column's real hazards:

```rust
assert_eq!(plain_text("Use Vec<String> here"), "Use Vec<String> here");
assert_eq!(plain_text("a < b and b > c"), "a < b and b > c");
assert_eq!(plain_text("a <b"), "a <b");
assert_eq!(plain_text("<pre-release plan>"), "<pre-release plan>");
```

That is knowledge about *this* data — that a description is sometimes prose containing a
less-than sign — and an HTML5 parser would not preserve it. The walker keeps it.

If a shape shows up that the walker gets wrong, the conversion is one function with tests
written from the real descriptions, and swapping `htmd` in is a one-function change.

### The hazard the walker inherits

Converting to markdown means the output is parsed *again*, so text that was inert as HTML
can become syntax on the second pass. The existing tests already name the shapes this
happens to:

```rust
assert_eq!(plain_text("Tom &amp; Jerry &lt;3"), "Tom & Jerry <3");
```

`&lt;3` decodes to `<3`, which the markdown parser may then read as the start of a tag;
decoded text carrying `*`, `_`, `#`, `` ` `` or `[` has the same problem, and so does a
line of prose that happens to begin `1. `. **Text content lifted out of HTML must be
escaped for markdown as it is emitted** — this is the part of the job `htmd` does
carefully and a naive walker does not do at all, and it is the most likely reason for the
one-function swap above.

Two consequences for the tests: the not-a-tag assertions move to the new function
unchanged in *input* but with markdown-escaped expectations where the round trip demands
it, and the escaping gets a test of its own driven by decoded entities rather than by tags.

## What changes

| file | change |
|---|---|
| `crates/tui-do-ui/src/markdown.rs` | new: events → `Vec<Line<'static>>`, themed and wrapped |
| `crates/tui-do-ui/src/rows.rs` | `plain_text` becomes `to_markdown`; its tag knowledge and its tests stay |
| `crates/tui-do-ui/src/view.rs` | `preview` renders lines instead of stripping tags; the `MAX_PARAGRAPH_LINES` budget survives |
| `crates/tui-do-ui/Cargo.toml` | `pulldown-cmark` |
| `PLAN.md` | Phase 5's markdown bullet rewritten; `markdown_renderer` and `doctor`'s markdown line removed |
| `CLAUDE.md` | a wire-format entry: a description is HTML *or* text, and which is not knowable from the spec |

## What this is not

Comments, relations and URL opening are Phase 5 and are not this. `tui-do doctor` may
still be worth building for other reasons — it has nothing to report about markdown once
there is no external binary to choose between.

## Testing

`tui-do-ui` is pure, so all of this is unit-testable in place and none of it belongs in
`tui-do-smoke` — there is no seam here, no store call and no message.

- One test per construct: heading, emphasis, code span, fence, list, blockquote, link,
  table.
- **A named regression test for decision 1**, using the Manatee County description,
  asserting the address lines do not collapse. If someone later "fixes" the soft break to
  be CommonMark-conforming, this is what tells them.
- HTML→markdown tests built from the shapes actually in the store, including the
  `<pre><code>` one and the `<p><a target="_blank" rel="noopener">` one.
- The four `plain_text` not-a-tag assertions, carried across unchanged.
- Wrapping at a narrow width, because the preview pane is narrow and it is the case the
  old code never had to handle.
