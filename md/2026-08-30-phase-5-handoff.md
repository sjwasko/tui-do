# Phase 5 — where to pick it up

Written 2026-08-30 at the end of the session that finished the label lifecycle and drove
`md/MANUAL-CHECKS2.md` section F. Read `CLAUDE.md` first; this only covers what that file
does not, which is *what to do next* rather than what is true.

**Correction, later the same day.** Everything below that names `glow` as the plan for
markdown descriptions is superseded: `glow` was measured and rejected, and the plan of
record is now comrak + html2text, built and shipped. See
`md/2026-08-30-markdown-descriptions-design.md`.

## Where the project actually is

Phases 1–4 are complete and driven by hand. The label lifecycle — create, rename,
recolour, from four surfaces — was built out of sequence between Phase 4 and Phase 5 and
shipped on 2026-08-29. Section F of the manual checks was driven on 2026-08-30 and passed,
after costing three changes to the code rather than to the checks:

- `C-n` cannot type a space, because Space is the tick key in the `l` form. Recorded in
  "What is known to be wrong", not fixed: two of the three creation routes take spaces.
- The colour field takes eight names now (`red`…`grey`, plus `gray` and `none`), because
  asking a person for six hex digits is asking the wrong question. Hex still works.
- `r` and `R` retry a queue entry inside its backoff; the timer and startup do not. Nothing
  else could tell tui-do that a network had come back.

The whole of section F is driven except F3, which has automated cover instead — see below.

## What Phase 5 still is

From `PLAN.md`, minus attachments, which were **dropped** on 2026-08-30 rather than
deferred (see the phase text for why).

| item | state |
|---|---|
| Markdown descriptions via `glow` | not started. `rows.rs:692` strips HTML tags and says in its own doc comment that it is a stopgap |
| Task detail pane | partial: `view.rs:388` shows fields, no description rendering, no comments |
| Comments | client only — `task_comments`, `update_comment`, `delete_comment` exist and are untested against a real server. Nothing in the UI |
| Subtasks and relations | client only — `Client::relate_tasks`, added 2026-08-30 for a live test. Nothing in the UI. cria has this half-built and disabled: do it properly or not at all |
| URL extraction and opening | not started. It is the first real user of the platform trait the macOS port depends on, so its shape matters more than its size |

Suggested order: markdown and the detail pane first (they are one piece of work and the
most visible), then relations, then comments, then URL opening.

## Things that will bite

**The store has no column for comments or relations.** Reminders needed schema v3 for
exactly this reason — a task read back out of a store that cannot hold a field goes to the
server carrying an empty one. Measured 2026-08-30: relations and attachments are *not*
replaced from an update body, so they do not need a column to be safe. Comments have not
been measured. Measure before writing, and add the answer to the table in `CLAUDE.md`.

**`glow` is an external Go binary.** `PLAN.md` specifies a `trait MarkdownRenderer` with a
`pulldown-cmark` fallback, config as `markdown_renderer: glow | builtin | auto`, and
`tui-do doctor` reporting which is active. The trait is not optional decoration: it is the
same seam the macOS port needs, and the fallback is what keeps tui-do working on a box
without `glow` — Ubuntu 26.04 does not ship it, Omarchy does.

**Rule 1 is the one that catches people.** `tui-do-ui` is pure and synchronous. Running
`glow` is a subprocess, so it is an `Effect` executed by the runtime, never a call from
`update`. A cache keyed by task id and width lives in the model; the effect answers with a
`Msg`.

**`crates/tui-do-smoke` exists for the seam neither crate can test alone.** It drives real
keystrokes through the real `update`, store and sync engine against a mock server. Its
harness defers the push until the reload has landed, mirroring the runtime, and getting
that order wrong makes the tests pass against broken code — the comment on `pending_push`
says how. Anything in Phase 5 that spans the layers belongs there.

## Before shipping

The GA bar is in `PLAN.md`. Two items are calendar, not code: a full day daily-driving
against dev and then against prod. Start those as soon as the code lands rather than after.

Publication prep, decided 2026-08-30:

- The README still says "Status: pre-alpha … Not yet usable", which has been false for
  days. Rewrite it once the feature set is frozen: what works, what does not, install,
  config, the key map, the WSL answer and macOS as post-GA.
- Tailnet hostnames stay. They are not credentials and they are not resolvable off the
  tailnet.
- `md/` keeps design documents and the manual checks; the ten session continuity notes
  were deleted on 2026-08-30. Do not start writing them again — a handoff like this one,
  when a session ends mid-phase, is the replacement.
- The dev host's directories were brought onto the `tui-do` name on 2026-08-30. Every `deploy/` script works again. `reset-dev.sh` now carries
  `api_tokens` across the restore — the baseline predates the token this workstation
  authenticates with, so a plain restore answered 401 from a server that was otherwise
  perfectly healthy.

## The state dev is in

Reset to the baseline on 2026-08-30 after section F was driven: 3,876 tasks, one label
(`test`), both API tokens intact. The baseline itself is a `pg_dump` from 24 August, so it
predates the label feature entirely — fine for task data, and it will never contain
anything from the week the labels were built. `snapshot-dev.sh` takes a newer one, but
only run it on a state worth returning to.

`md/MANUAL-CHECKS2.md` F1 needs an **empty** label pool and the baseline has one label in
it, so F1 is not reachable without deleting `test` in the web UI first. That is the only
check in section F that has never been driven apart from F3.

## What is not covered by anything automated

`md/MANUAL-CHECKS2.md` F3 — that a label created in the open `l` form is renumbered when
the server names it. `crates/tui-do-smoke/tests/labels.rs` covers the seam, but the harness
reimplements the effect runtime rather than running it, and a mock is not Vikunja. It has
never been driven by hand.
