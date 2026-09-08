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

**1. Wait on the OAuth spec PR, then refresh `spec/vikunja.json`.** *(raised 2026-09-08)*
[`go-vikunja/vikunja#3837`](https://github.com/go-vikunja/vikunja/pull/3837) is **open** and
adds `@Router` annotations for `/oauth/authorize` and `/oauth/token` plus the regenerated
`pkg/swagger` — 126 paths to 128, 521 insertions and no deletions. Nothing to do here until
it merges *and* a server carrying it is deployed; then `cargo xtask fetch-spec`, because
Rule 3 checks tui-do's pinned copy and not upstream's. That is the prerequisite for
anything below that touches OAuth.

**2. Drive the rest of the interface on macOS.** *(raised 2026-09-07)*
The port note's honest limit: the compiler, the test suite and the startup path are proven,
and `o` was confirmed by hand. Rendering, keybindings, sync against a live server from
inside the interface, OSC 52 in Terminal.app and iTerm2, lid close, and `o`'s *copy* branch
over SSH are all undriven. The plan is written — `md/2026-09-08-macos-test-plan.md`,
2026-09-08 — and waits on the machine being stood up. It opens by flagging that
`README.md:500` already claims "several terminals, tmux, and over SSH", none of which has
been driven; that claim either gets backed or gets cut.

---

## Next — the agent surface

This is where the 2026-09-08 competitive analysis (`md/2026-09-08-veans-competitive-analysis.md`)
lands, and the order matters.

**3. An MCP server, and read verbs with `--json`. One piece of work, not two.**
*(raised 2026-09-08; in progress on `feature/mcp-server`, worktree `../tui-do-mcp`)*
Design taken in `md/2026-09-08-mcp-server-design.md`. `tui-do list` and `tui-do show` are
still the item — today `add` is the entire agent surface, and the real gap against veans is
not quick-add versus flags but that veans can read and tui-do cannot. Answering **from the
local store** is the part veans structurally cannot copy.

**They merged into one item because they are one contract.** An MCP tool result and
`tui-do list --json` are the same promise about the same shape through different doors.
`PLAN.md` and `skills/tui-do/SKILL.md` both flag that shape as undecided and warn that once
something depends on it, it is an API; shipping the two separately means two incompatible
agent-facing contracts and a reconciliation that breaks whichever arrived first. So one
serialisation module — `tui_do_core::agent` — is the single source and both surfaces render
from it.

**Nothing of this is on `main` yet, deliberately.** The branch also carries the
`runtime::add` split that `bugs.md` §2 already prescribes, because MCP speaks JSON-RPC over
stdout and a function that prints six branches of console report cannot be called from it.

**4. Kanban buckets.** *(raised 2026-09-08; on the roadmap since Phase 5)*
Promotes from "Vikunja parity" to "the feature that makes the agent story real".

**Two corrections from the 2026-09-08 MCP design, both from `PLAN.md`'s own post-GA
sections.** First, the *write* side is **not** blocked on tui-do rendering a board — *"the
web UI is the display and tui-do is the actuator"*, and the write side alone delivers most
of the value and can ship first. The real blocker is narrower: **`tui-do-api` has no bucket
calls at all**, so this is "a client method, a `Mutation`, and a CLI verb, not a new
subsystem."

Second, **a bucket belongs to a view, not to a project**, and the CLI shape has to make that
unmissable or someone moves a card on a board nobody is looking at. The MCP server therefore
carries a view in its binding from its first commit even though nothing reads it yet, so the
label-to-bucket swap cannot change a launch contract agents have already been configured
with.

**5. A review queue.** *(raised 2026-09-08; unblocked early by item 3)*
A view filtered to work an agent has finished and parked for a human. Item 3's status labels
(`agent:in-review`) give this a filter to read **before** buckets exist, so it no longer
waits on item 4. veans's one good rule
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

- `README.md:500` claims macOS was tested across several terminals, tmux and over SSH; none of that has been driven. See item 2.
- The three structural troubleshooting surfaces in `bugs.md` (`sync::push_with`,
  `runtime::add`, `update::apply_edit`) are still the worst places to debug. **`runtime::add`
  is scheduled**: item 3's branch splits it into `resolve_or_explain` and `report` exactly as
  `bugs.md` §2 prescribes, because an MCP server cannot call a function that writes to the
  stdout it speaks JSON-RPC over. The other two are untouched.

---

## Done

- **2026-09-08** — **Vikunja PR #3837 opened**: swagger annotations for the two OAuth 2.0
  endpoints, which are served and were in none of the 126 documented paths. Regenerated
  with `mage generate:swagger-docs`; `mage lint` clean, `gofmt` clean, the package tests
  pass. Two corrections to the drafted annotations came out of reading their source rather
  than assuming: the token endpoint is `@Accept json`, matching upstream's own comment that
  v1 binds JSON and v2 takes the form body (v1 *does* bind a form body — measured — and the
  PR hands them that fact rather than deciding it for them); and authorize's `403` is
  `models.Message`, not `web.HTTPError`, because it comes from `echo.NewHTTPError` with a
  string, which `error_handler.go` wraps as `{"message": …}`. The handoff's fallback import
  path for `web` was also wrong — it is `code.vikunja.io/api/pkg/web` — but `swag` resolved
  the bare `web.HTTPError` through `--parseDependency`, so it was never needed.
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
