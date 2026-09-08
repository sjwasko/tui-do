# tui-do — what is planned, and what is open

**This is the live backlog. Any session picking up work starts here.**

`PLAN.md` is the foundation plan and is historical — phases 0–5 are done and it does not
describe what comes next. `bugs.md` holds defects. This file holds **intent**: features
agreed, decisions pending, and debt acknowledged. If something is planned and it is not in
this file, it is not planned; add it here rather than leaving it in a chat log or a design
note nobody re-reads.

Keep it honest. An item moves to **Done** when it ships, and a decision moves to a design
note in `md/` when it is taken. Items carry the date they were raised.

Last touched 2026-09-08.

---

## Now

**1. PR the missing OAuth paths to Vikunja's OpenAPI spec.** *(raised 2026-09-08)*
`/api/v1/oauth/authorize` and `/api/v1/oauth/token` are live on the server and absent from
all 126 paths in `docs.json`, because `pkg/modules/auth/oauth2server/{token,authorize}.go`
carry no `@Router` annotations. Filed against `go-vikunja/vikunja`. This is a prerequisite
for anything below that touches OAuth, because of Rule 3.

**2. Drive the rest of the interface on macOS.** *(raised 2026-09-07)*
The port note's honest limit: the compiler, the test suite and the startup path are proven,
and `o` was confirmed by hand. Rendering, keybindings, sync against a live server from
inside the interface, OSC 52 in Terminal.app and iTerm2, lid close, and `o`'s *copy* branch
over SSH are all undriven. **`md/2026-09-07-macos-test-plan.md` is referenced by the port
note and does not exist** — write it or drop the reference.

---

## Next — the agent surface

This is where the 2026-09-08 competitive analysis (`md/2026-09-08-veans-competitive-analysis.md`)
lands, and the order matters.

**3. Read verbs, with `--json`.** *(raised 2026-09-08)*
`tui-do list` and `tui-do show`. Today `add` is the entire agent surface, and the real gap
against veans is not quick-add versus flags — it is that veans can read and tui-do cannot.
Answering **from the local store** is the part veans structurally cannot copy: instant, and
works offline. Smaller than Kanban and worth doing first.

**4. Kanban buckets.** *(raised 2026-09-08; on the roadmap since Phase 5)*
The blocker for everything below it: tui-do cannot currently display the thing an agent
moves. Promotes from "Vikunja parity" to "the feature that makes the agent story real".

**5. A review queue.** *(raised 2026-09-08)*
A view filtered to work an agent has finished and parked for a human. veans's one good rule
is that the agent never closes its own task — that leaves a sign-off step whose only home
today is a web browser. Highest value per unit of work on this list, and a feature an
agent CLI structurally cannot build.

**6. An agent inbox.** *(raised 2026-09-08)*
A project or label that is an agent's feed: specs and tasks filed to it by a human, picked
up by the agent. Needs nothing new on the wire — a saved filter plus a convention.

---

## Auth and credentials

**7. Decide: OAuth first, or keychain first.** *(raised 2026-09-08 — blocks 8)*
Not decided, and it should be decided before either is built. `tui-do login` that completes
an OAuth flow and stores the result in the keychain is **one** feature with one shape;
building the storage half first is how the subcommand ends up wrong. See
`md/2026-09-07-credential-storage-design.md`, including its 2026-09-08 correction.

Two things stand in the way of OAuth and neither is the server: **Rule 3** (the endpoints
are real and undocumented — item 1) and **a browser in the loop**, which is a new
interaction on the startup path where Rule 1 says no prompts.

**8. Keychain credential storage.** *(raised 2026-09-07 — blocked on 7)*
Designed, not built. Strictly additive: keychain → `TUI_DO_API_TOKEN` → `token_file` →
inline. The open measurement is whether a libsecret backend survives a static musl build;
if it does not, the Linux half may be from-source only. **Do not start before 7.**

---

## Announced publicly, and therefore owed

From the launch post on `community.vikunja.io`, 2026-09-07. These were said out loud, so
they are commitments rather than ideas.

| | status |
|---|---|
| macOS port | **done** — `v1.0.1`, plus a Homebrew tap. Item 2 is the remaining honesty gap |
| Agentic AI | items 3–6 |
| Comments | not started |
| Subtasks | not started |
| WSL support | not started; Windows is currently answered with "use WSL" |
| A standalone version, with no Vikunja backend | not started, and the largest of these by far — it means a second source of truth behind the store, and it should get a design note before any code |

---

## Open defects

Tracked in full in `bugs.md`; listed here only so this file is a complete picture.
BUG-7 and BUG-9 need a decision, BUG-14 needs a measurement, BUG-2 is accepted and will
not be fixed. None are Critical.

---

## Documentation debt

- `md/2026-09-07-macos-test-plan.md` is referenced and absent — see item 2.
- The three structural troubleshooting surfaces in `bugs.md` (`sync::push_with`,
  `runtime::add`, `update::apply_edit`) are still the worst places to debug.

---

## Done

- **2026-09-08** — README's first-time setup now covers macOS directly rather than making a
  Mac user translate Linux paths, and both config examples use a relative `token_file`,
  which is portable across platforms and machines.
- **2026-09-08** — **tui-do is listed on Vikunja's External Integrations page.** PR #399,
  invited by kolaente on the announcement thread and merged by him the same day; live at
  `vikunja.io/docs/integrations/`.

  Worth keeping: the first push carried the PR title and body *inside* the file, every
  line indented two spaces, because the entry and the PR metadata were handed over as one
  block and pasted together. kolaente requested changes on it. That is the paste-mangling
  failure in CLAUDE.md's "Driving another box by hand", arriving somewhere that section
  does not cover — a browser textarea rather than a remote terminal. The fix was the same
  one that section already prescribes: build the file and move it, do not retype it.
- **2026-09-08** — the credential note's "OAuth is unavailable" finding corrected: the
  authorization server is live on `v2.5.0`.
- **2026-09-07** — macOS port, `v1.0.1`; crates.io names claimed; the naming question
  against `gouveags/tui-do` settled in favour of keeping it.
