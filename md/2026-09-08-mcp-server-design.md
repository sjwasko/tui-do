# An MCP server for tui-do

**Written 2026-09-08.** Built on `feature/mcp-server`, in a worktree at
`../tui-do-mcp`, and deliberately not on `main` until the questions in §13 are answered.

The goal in one line: **an agent files, reads and progresses tasks through tui-do rather
than around it**, so it inherits the local store, the outbox, the backoff and
`Task::merge_onto` instead of becoming a second writer with none of them.

`md/2026-09-08-veans-competitive-analysis.md` is the prior art and its conclusion stands:
do not build a competing agent CLI. This is not one. veans owns the agent's hands; tui-do
owns the human's seat. An MCP server is how the agent reaches tui-do's seat without a
second copy of tui-do's guarantees.

---

## 1. Why MCP rather than more CLI

`PLAN.md` §"Post-GA — the CLI as the agent surface" already argues that every caller should
take the same path, and it is right. This does not replace that argument — §10 below builds
the CLI read verbs as part of the same work, because they are the same contract.

What MCP adds over a CLI is not capability, it is **delivery of the workflow rules**.

veans has to re-emit its system prompt with `veans prime` on every session start and after
every context compaction, because the prompt is text on stdout and nothing in the shell
remembers it. MCP carries `instructions` in the `initialize` response and the harness owns
their lifetime. The same intent, without the re-priming, and without depending on an agent
remembering to run a command before it starts.

The second thing it adds is **enforcement by schema**. veans documents "the agent never
closes its own task". Here `done` is simply absent from the enum the agent can send, so it
is not a rule that survives or fails to survive a compaction — it is a value that cannot be
expressed. That distinction is the single strongest reason this is worth building as a
protocol surface rather than as more flags.

## 2. Where it lives

A worktree at `/home/swasko/code/tui-do-mcp` on branch `feature/mcp-server`, so the main
checkout is untouched and merging later is a real `git merge`.

**Not a gitignored directory**, which is what was first proposed and which does not work: an
ignored directory is committed nowhere, so there is no branch, no history, and a
`git clean -xdf` deletes the feature. `CLAUDE.md` already records that failure —
`md/ruflo-audit/`, `md/bloat-detector/` and `md/minify/` sat untracked for days and one held
a credential. The goal behind the request was "do not disturb `main`", and a branch is the
tool for that.

New crate `crates/tui-do-mcp`, added to the workspace member list **on the branch only**. It
is shaped publishable from the first commit — `version`, `description`, `license`,
`repository`, and explicit versions on its path dependencies, because `deny.toml` sets
`wildcards = "deny"` and a versionless path dependency reads as `*`. Discovering that at
merge time is how a merge stalls on a lint nobody expected.

## 3. Substrate

Rust, linking `tui-do-core` directly.

```
tui-do-mcp ──▶ tui-do-core   store, outbox, sync, quickadd, config
           ──▶ tui-do-ui     quickadd_task -- pure name resolution
           ──▶ rmcp          protocol
           ──▶ tokio
```

No `ratatui`, no `crossterm`. **Rule 1 is untouched**: `tui-do-ui` gains no dependency, and
the ban that enforces the rule is a statement about that crate's own dependency list.

The dependency on `tui-do-ui` looks wrong for a server and is not. `quickadd_task` resolves
a parsed line against the project and label lists and is pure; `crates/tui-do` already
reaches for it the same way from `runtime::add`. Duplicating it would give tui-do two
name-resolution behaviours to keep in agreement, which is worse than an odd-looking arrow.

`rmcp` 3.2.0 is the official `modelcontextprotocol/rust-sdk`, Apache-2.0 — already on
`deny.toml`'s allow list — 12.7M recent downloads, last published 2026-08-31. Checked rather
than assumed, because a protocol SDK that turns out to be abandoned is a rewrite.

**A non-Rust server was rejected.** A TypeScript or Python server either shells out to
`tui-do`, paying a process spawn per call and inheriting whatever the CLI happens to expose,
or it talks to Vikunja's REST API directly — at which point it has no local store, no
outbox, no offline, no merge, and it is veans with a worse distribution story. tui-do
already ships static musl binaries for two Linux architectures and Apple Silicon; `tui-do
mcp` is distributed the day it merges, and an npm package is a second install story for a
project whose whole pitch is one binary.

## 4. The tool surface

veans's verbs are the template, per the decision of 2026-09-08. Where a mapping is refused,
the reason is a project rule and is recorded.

| veans | tui-do MCP | note |
|---|---|---|
| `list` | `list_tasks` | local store, offline |
| `show <id>` | `get_task` | local store |
| `create` | `add_task` | quick-add line **or** structured fields |
| `update <id>` | `update_task` | title, description, priority, due, labels |
| `update -s` | `set_task_status` | see §6 |
| `claim <id>` | `claim_task` | assign, `in_progress`, git branch tag |
| `prime` | server `instructions` + an MCP prompt | §1 |
| — | `sync` | force a full pull; no veans equivalent |
| `update --comment` | **not built** | comments do not exist in tui-do; `md/TODO.md` records them as owed and not started |
| `api METHOD PATH` | **refused** | §4.1 |
| `init`, `login`, `version` | not applicable | the harness owns launch; config already exists |

### 4.1 Why the raw API escape hatch is refused

veans offers `veans api METHOD PATH` as a passthrough. tui-do will not.

**Rule 3** says no endpoint is called that isn't in `spec/vikunja.json`, and a conformance
test asserts every path template the client builds exists there. A passthrough builds its
path from an agent-supplied string at runtime, so the assertion becomes unmakeable — not
merely inconvenient, but structurally impossible to state.

The second reason is larger. A raw call bypasses the outbox, so it is not optimistic, not
retried, not rolled back on rejection, and not merged onto the server's current copy. It is
exactly the "second writer with none of that" `PLAN.md` warns about, offered as a
convenience from inside the tool whose entire value is the opposite.

An agent that genuinely needs an endpoint tui-do lacks should cause that endpoint to be
added. That is slower and it is the right slowness.

## 5. Reads answer from the local store

`list_tasks` and `get_task` never touch the network. This is the part veans structurally
cannot copy, and `TaskFilter { project, done, label, favorite, search, limit }` plus ten
sort orders already exist in `tui-do-core::store::tasks` — `PLAN.md`'s claim that the query
logic "simply has no CLI surface" was checked and holds.

`PLAN.md` left an open question here — *"Whether `ls` pulls first. Instant reads are the
point, so probably not — but then a stale store answers, and an agent has no way to ask for
freshness without a flag that costs 78 pages."* **This design answers it, and the answer is
cheaper than the question assumed:**

- Reads never pull. Instant and offline, always.
- Every read response carries `synced_at`, so an agent can *see* staleness instead of
  guessing at it.
- The server runs the same incremental pull timer the interface does. An incremental pull is
  **one page**, not 78 — the 78-page cost is a `Full` reach only. So the store stays fresh
  while the server is alive at almost no cost, which is the half the open question missed.
- A `sync` tool forces a `Full` pull, passing `Backoff::Ignore` and `Trigger::Asked` — the
  existing `R` machinery, not a new path.

**One wrinkle found while checking.** `TaskFilter::label` is a single `Option<LabelId>`, so
"every task carrying any `agent:*` label" is not one query. Either the filter grows a set or
the review-queue read runs one query per status and merges. Decide in the plan; it does not
change the tool contract.

## 6. Status, and the one rule worth stealing

Decided 2026-09-08: **labels now, buckets later, and the agent never learns which.**

```
status: todo | in_progress | in_review | scrapped
```

behind a `StatusBackend` trait. `LabelBackend` today, writing `agent:todo`,
`agent:in-progress`, `agent:in-review`, `agent:scrapped`. `BucketBackend` later, mapping the
same enum onto real Kanban buckets. The tool schema is identical across the swap, so no
agent configuration and no prompt changes when buckets arrive.

The `agent:*` labels are a fixed, known set, so the server creates them on demand. This does
**not** contradict the `--create-labels` caution in `README.md` and `skills/tui-do/SKILL.md`
— that caution is about a *model-generated* label name becoming a permanent entry in a
global pool. These four names are chosen here, not by a model, and they are the same four on
every machine.

**`done` is not in the enum.** veans's best idea is that the agent parks work in review and
a human signs it off; expressing it as an absent value rather than as documented guidance is
the whole argument of §1.

This is consistent with, not contradicted by, the planned `tui-do done <id>` CLI verb in
`PLAN.md` and `skills/tui-do/SKILL.md`. The CLI is the human's hand and may close a task.
MCP is the agent's, and may not. Said plainly here because unstated it reads as a
contradiction.

### 6.1 This vocabulary is provisional, and merging is what would settle it

`README.md` says, in the shipped release:

> What an agent should *say* to move a task across a board — name a bucket, set a status,
> ask for "the next column" — is genuinely undecided … I would like your opinion before it
> is built rather than after.

That invitation is public and it is still open. Building a vocabulary on an unmerged branch
is how an informed opinion gets earned; **merging it to `main` is the act that would settle
a publicly-open question by fiat.** So the enum above is a proposal with running code behind
it, and the merge gate in §13 includes resolving that invitation rather than quietly
overtaking it.

## 7. Identity is an indirection from day one

Decided 2026-09-08: acts as the user today, a real Vikunja bot user later.

```rust
enum AgentIdentity {
    ActingAsUser { .. },   // today
    Bot { user_id, .. },   // planned
}
```

`PLAN.md` already framed this exact fork — *"either each agent gets an account or assignment
is carried by a label instead. The first is more honest and costs more."* Acting as the user
is the cheaper half, chosen knowingly, and the enum is the seam the honest half swaps into.

The cost of the bot today is concrete and worth recording: the local SQLite store is scoped
to one server-and-user pair, so a bot identity means a **second store, a second token and a
second sync loop** — the offline advantage, which is the entire reason this is Rust, paid
for twice.

Attribution today is the `agent:*` labels plus the bound project. Deliberately **not** a
label per agent name: labels are one global pool and per-agent names proliferate in exactly
the way `--create-labels` exists to prevent.

## 8. Scope: the binding

The server is launched bound to a project and refuses to start without one. Every read
filters to it; every write forces `project_id` into the body, per the path-loses-to-body
finding in `CLAUDE.md`. Binding rejects `id <= 0`, because `/projects` returns pseudo-projects
that reject writes.

**The binding carries a view, not only a project, from the first commit** — even though
nothing reads it yet. `PLAN.md` and `skills/tui-do/SKILL.md` both record that **a bucket
belongs to a view, not to a project**, with the warning that *"the CLI shape has to make
that unmissable or someone will move a card on a board nobody is looking at."* veans's
`.veans.yml` carries `view_id` alongside five bucket ids for the same reason. If the binding
were project-only, the `LabelBackend` → `BucketBackend` swap would change the launch
contract and break every agent configuration already written — which is precisely the thing
§6 promises it will not do.

```
"args": ["mcp", "--project", "tui-do"]              today
"args": ["mcp", "--project", "tui-do", "--view", "Board"]   later, additive
```

## 9. stdout is the protocol

MCP over stdio uses stdout for JSON-RPC. One stray `println!` and the agent sees a parse
error rather than a task.

So `crates/tui-do-mcp` keeps the workspace `print_stdout = "deny"` and `print_stderr =
"warn"` lints rather than blanket-allowing them as `crates/tui-do/src/main.rs` does. **The
lint is the enforcement mechanism**, the same way the dependency ban enforces Rule 1 rather
than a convention anyone has to remember.

This has a direct consequence for reuse: `runtime::add` prints six branches of console
report, so the MCP server cannot call it as it stands. It is split into a value-returning
core and a formatting shim.

**That split is scheduled debt, not a tax this feature invents.** `bugs.md` §2 already lists
`runtime::add` — 148 lines, five jobs — under Structural, with the fix prescribed: *"Extract
`resolve_or_explain(...) -> Result<Built>` and `report(...)`; `add` drops to ~50 lines."*
This work does that, with that naming. `md/TODO.md`'s documentation-debt section names the
same function as one of the three worst places to debug.

## 10. One JSON contract, two doors

**Decided 2026-09-08: `md/TODO.md` item 3 is built on this branch, from a shared core.**

The MCP tool results and `tui-do list --json` are the same promise about the same shape,
arriving through different doors. That the contract is undecided is flagged in three
separate places:

> `PLAN.md`: "What `--json` promises. A stable shape is an API… Worth deciding whether it is
> versioned or explicitly unstable."
>
> `skills/tui-do/SKILL.md`: "The --json contract is undecided: once something depends on the
> shape, it is an API."
>
> `md/TODO.md` item 3: `tui-do list` and `tui-do show`.

Shipping MCP tool shapes now and CLI `--json` later means **two incompatible agent-facing
contracts** and a reconciliation that breaks whichever arrived first. So one serialisation
module in `tui-do-core` is the single source of the task shape, and both surfaces render
from it:

```
tui_do_core::agent          -- the module, new in this work
      TaskView { id, title, status, project, labels, due, priority, ... }
                    │
          ┌─────────┴─────────┐
     MCP tool result     tui-do list --json
```

Whether that shape is versioned or declared explicitly unstable is **still open** and is
listed in §13. Deciding it is now unavoidable, which is an improvement on deciding it by
accident.

## 11. Writes

`add_task` and `update_task` go through `Store::queue` — the same single transaction that
writes the local row and the outbox entry, so the two cannot disagree.

**An unreachable server returns success with `queued: true`, not an error.** This is the
behaviour `README.md` and `SKILL.md` already promise for `tui-do add` — *"A zero exit means
the task is in the local store — not that the server has it yet. That is the design, not a
limitation"* — and it is what makes an agent on a long unattended run not lose work.

**Provisional ids are an open problem, not a solved one.** A task created offline is given a
negative id and adopted on push by `settle_create`. An agent handed `-3`, which later
becomes `812`, needs its handle to survive. Whether the store keeps a durable mapping is
**unverified**, and §13 carries it. If it does not, the tools return a stable handle rather
than a raw id. This is recorded as unknown rather than assumed, because assuming it and
being wrong produces an agent that silently edits the wrong task.

## 12. Testing

Following `CLAUDE.md`'s rule about where a test that spans layers goes: `tui-do-mcp` depends
on `tui-do-core` directly, so it can hold its own cross-layer tests without `tui-do-smoke`.

- **Tool behaviour** against a real temp store, with `wiremock` for the push half — the
  pattern `tui-do-api` already uses.
- **Protocol integrity.** The server driven over an in-memory duplex transport, asserting
  JSON-RPC in and out. This is the test that catches a corrupted stdout, which is this
  crate's characteristic failure and one that no unit test can see.
- **Golden JSON** for every tool response, so a change to the agent-facing contract shows up
  in a diff. The golden screens do this for the interface; the agent contract in §10
  deserves the same treatment, and more so, because a screen regression is visible and a
  reshaped JSON field is not.
- **A test that `done` cannot be sent**, which is the §6 rule expressed as an assertion.

## 13. Open, and what gates a merge

1. **Is the `--json`/tool shape versioned, or explicitly unstable?** §10. Must be answered
   before anything depends on it, which is the moment it merges.
2. **Do provisional ids survive as agent-visible handles?** §11. A verification, not a
   decision — settle it against the code during planning.
3. **The public invitation on status vocabulary.** §6.1. Open in `README.md`; merging
   overtakes it.
4. **What a stuck run looks like.** `PLAN.md` flags that nothing distinguishes "still
   working" from "the agent died holding it" and suggests a last-touched time or heartbeat
   label as the smallest fix. `claim_task` is where it belongs and this design does not yet
   handle it.
5. **Comments and subtasks.** Both are publicly owed from the launch post, both are in the
   veans surface used as the template (`--comment`, `--parent`, `--blocked-by`), and neither
   exists in tui-do. Stated as a gap rather than papered over.
6. **`publish` at merge.** If `tui-do` depends on `tui-do-mcp` for the `mcp` subcommand, the
   crate must be published like the other three, from the tag rather than from `main`.

## 14. Explicitly not in this branch

- The `BucketBackend`. Decided 2026-09-08: the trait and the view-carrying binding ship, the
  implementation does not. Worth recording *why* it is deferred, because the obvious reason
  is wrong: it is **not** blocked on tui-do rendering a Kanban board. `PLAN.md` is explicit
  that *"the web UI is the display and tui-do is the actuator"* and the write side can ship
  first. The real blocker is narrower — **`tui-do-api` has no bucket calls at all** — and
  the work is "a client method, a `Mutation`, and a CLI verb, not a new subsystem."
- A real bot user. §7.
- HTTP transport. stdio only. HTTP is additive later and is the interesting question for the
  Tailscale fleet, where an agent on one box might drive a store on another.
- Comments, subtasks, relations.
- `veans api` passthrough, permanently. §4.1.
