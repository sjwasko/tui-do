# tui-do — what is planned, and what is open

**This is the live backlog. Any session picking up work starts here.**

`PLAN.md` is the foundation plan and is historical — phases 0–5 are done and it does not
describe what comes next. `bugs.md` holds defects. This file holds **intent**: features
agreed, decisions pending, and debt acknowledged. If something is planned and it is not in
this file, it is not planned; add it here rather than leaving it in a chat log or a design
note nobody re-reads.

Keep it honest. An item moves to **Done** when it ships, and a decision moves to a design
note in `md/` when it is taken. Items carry the date they were raised.

Last touched 2026-09-13.

## The road map, as of 2026-09-13

Three questions were argued out on 2026-09-13 and their answers are in this file rather
than in a chat log. Read in this order, because each one assumes the last:

| the question | where the answer is |
|---|---|
| **1. Auth** — what are we actually building, and why | item 9, then "The macOS auth goal" |
| **2. The lease** — concurrent agents, identity, scaling | "How this scales — the five tiers" |
| **3. MCP** — how it authenticates, and what breaks at two processes | item 10, with its test design |

**The critical path is item 3 (the MCP server), and item 10 gates its first write.**
Everything in the launch section is held for item 3 shipping. Items 4–6 follow it. Items
8 and 9 are independent of all of that and are the shortest route to a visibly better
product on macOS. **Which of those to do first is settled in "Where to start" below** —
read that before picking anything up.

A plain-English version of question 1 is in the knowledge base at
`3-developer/tui-do-auth-explainer.md`, and of question 2 at
`md/2026-09-08-mcp-process-model.png` plus the five-tier section below.

---

## Where to start — development priority, set 2026-09-13

**A session picking up work starts here, not at "## Now".** That heading holds items 1 and
2 for historical numbering; item 1 is blocked on upstream and item 2 is done. This section
is the actual order. Items keep their stable numbers — nothing is renumbered, because other
notes cite them — so this says *what order*, not *what number*.

```
  NOW      10(a)+(b)  two-process fixes + the test        ~1 session
           9          tui-do login                        ~1-2 sessions
           8(macOS)   keychain, behind a cfg              ~1-2 sessions
           ------------------------------------------------------------
           delivers the stated macOS goal in full, plus the foundation
           everything agentic needs

  NEXT     3a         tui_do_core::agent + list/show --json
           3b         the MCP server, rendering from that same module

  DECIDE   BUG-20 before November. The --json contract before 3a ships.
           BUG-7 and BUG-9 need a decision, not work.

  DEFER    4 (Kanban), standalone mode, comments, subtasks
```

**Why 10(a) and (b) go first.** Both are small, certain, and **silent when wrong**. The
transaction change is one line; the sequential-write decision in the MCP crate is free if
taken deliberately and expensive to retrofit. Skipped, they surface weeks later as a
corrupted store or a duplicated task with no obvious cause. And (b) is a real defect
*today*, not only under MCP — `tui-do add` alongside the open interface has been two
processes on one file since Phase 4.

**Why 9 and 8 come before the critical path.** They are the whole of the stated macOS goal,
they are unblocked by anything, they carry no open design question, and they are
**finishable** — one or two sessions each, where item 3 is several before anything works at
all. They also improve the first thing a `brew install` user meets, which matters more than
it looks at 27 unique visitors in 14 days: the few people who do arrive should not hit a
`printf`/`chmod` wall. Take **only the macOS half of item 8** — the 53 crates and the second
async runtime are Linux's problem, and Linux mostly cannot use a keychain anyway.

**The judgement call, and it is reversible.** Putting auth before item 3 costs launch time:
the one-shot channels are all held for the MCP server, and the differentiated story is the
agent one, not the TUI one. **Inverting the NOW and NEXT blocks is legitimate** and should be
done without ceremony if launch timing starts to matter more than the macOS experience. The
reason it is written this way round is that a project built in evenings is better served by
what can be *finished* than by what is theoretically highest-value.

**Split item 3, which its own design note does not.** §8 is right that the MCP tool result
and `tui-do list --json` are one contract through two doors — but one contract does not mean
one shipment. **3a** is `tui_do_core::agent` plus the CLI read verbs: no rmcp, no new crate,
no protocol layer, useful the day it lands, and it closes the "veans can read and tui-do
cannot" gap on its own. **3b** is the MCP server rendering from that same module. The hazard
§8 warns about is two *serialisations* drifting; shipping one door first creates one contract
with one consumer, then two. This halves the risk of the largest item on the list.

**Note item 3 starts from nothing.** The branch is an ancestor of `main` and the worktree is
stale — rebase it or cut a fresh one before writing in it.

### Why the deferrals, said out loud

- **Kanban (item 4).** Agent-state-as-labels is fine at n=1; build buckets when the global
  label pool actually chafes, which is Tier 4. It is also the question `README.md` asked the
  public, and merging MCP with a label scheme closes it quietly. Ship 3a/3b, see whether
  anyone engages, then decide.
- **Standalone mode.** The largest owed item, wants a design note before code, and has one
  unanswered question that determines all the others ("does the outbox stay armed?"). No
  demand signal.
- **Comments and subtasks.** Publicly owed, genuinely not asked for, and `PLAN.md` has
  already reasoned out why deferring loses nothing — `related_tasks` is measured as *not*
  replaced from an update body, so the data survives while they wait.

### The caveat that outranks this whole section

All of it optimises for **a single user with agents**, which is the honest scope today. If
someone else turns up wanting tui-do, their first request will probably not be on this list,
and it should outrank this ordering.

---

## Now

**1. Wait on the OAuth spec PR, then refresh `spec/vikunja.json`.** *(raised 2026-09-08)*
[`go-vikunja/vikunja#3837`](https://github.com/go-vikunja/vikunja/pull/3837) is **open** and
adds `@Router` annotations for `/oauth/authorize` and `/oauth/token` plus the regenerated
`pkg/swagger` — 126 paths to 128, 521 insertions and no deletions. Nothing to do here until
it merges *and* a server carrying it is deployed; then `cargo xtask fetch-spec`, because
Rule 3 checks tui-do's pinned copy and not upstream's. That is the prerequisite for
anything below that touches OAuth.

**2. ~~Drive the rest of the interface on macOS.~~ Done 2026-09-13** — see the Done section.
The number stays rather than renumbering: `md/2026-09-08-mcp-server-design.md` and item 5
both cite items by number, and shifting them would break those references silently.

---

## Next — the agent surface

This is where the 2026-09-08 competitive analysis (`md/2026-09-08-veans-competitive-analysis.md`)
lands, and the order matters.

**3. An MCP server, and read verbs with `--json`. One piece of work, not two.**
*(raised 2026-09-08; designed, **not started**)*
Design taken in `md/2026-09-08-mcp-server-design.md`. `tui-do list` and `tui-do show` are
still the item — today `add` is the entire agent surface, and the real gap against veans is
not quick-add versus flags but that veans can read and tui-do cannot. Answering **from the
local store** is the part veans structurally cannot copy.

**Nothing is built, and this entry used to say otherwise.** Checked 2026-09-13:
`feature/mcp-server` is an *ancestor* of `main` — no commits of its own, six behind — and the
worktree at `../tui-do-mcp` holds the same five crates `main` does, with no
`crates/tui-do-mcp`. Forgejo and GitHub both answer `418d8aa` for that branch, matching
local, so this is not a mirror lagging: the branch genuinely carries nothing. **A GitHub
branch page renders the whole repository at its commit, which is why an empty branch looks
like work** — the "6 behind, 0 ahead" count is the part that says otherwise, and it is worth
knowing before the next branch is judged by eye. The worktree is still the right home for
this; rebase it or cut a fresh branch before writing in it.

**They merged into one item because they are one contract.** An MCP tool result and
`tui-do list --json` are the same promise about the same shape through different doors.
`PLAN.md` and `skills/tui-do/SKILL.md` both flag that shape as undecided and warn that once
something depends on it, it is an API; shipping the two separately means two incompatible
agent-facing contracts and a reconciliation that breaks whichever arrived first. So one
serialisation module — `tui_do_core::agent` — is the single source and both surfaces render
from it.

**It carries the `runtime::add` split with it**, which `bugs.md` §2 already prescribes
independently: MCP speaks JSON-RPC over stdout, and a function that prints six branches of
console report cannot be called from it. That is scope, not a second item.

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


**10. Two processes on one store file — what MCP makes ordinary.**
*(raised 2026-09-13; gates the first MCP write, not the MCP design)*

Item 3 puts N `tui-do mcp` processes on one SQLite file. Several assumptions in the
codebase hold for one process and stop holding at two. None of this changes the MCP
design; all of it has to be true before an MCP server writes to a real server.

**First, the part that needs no work at all: MCP over stdio has no authentication.** The
client *spawns* `tui-do mcp` as a child process and talks over pipes — no port, no socket,
no handshake. The trust model is "if you can spawn the process, you are the user": the
child inherits the OS account, the environment, the config and the token. So Gemini CLI,
Claude Code and Hermes side by side are **necessarily the same Vikunja user**, and it is
not a choice anyone gets to make. Only a different `TUI_DO_CONFIG` separates them.

That is free today and stops being free with the HTTP daemon, which genuinely cannot tell
who is calling — the MCP spec defines OAuth 2.1 for HTTP transports for exactly that
reason. Worth knowing before the daemon is scoped, not during.

**What actually needs doing, cheapest and most certain first:**

**(a) The MCP crate writes sequentially, by construction.** `bugs.md` BUG-2 is an
18.3%-at-0µs write-reordering race, accepted and not fixed — and read *why* it was
accepted: "two distinct user actions, which are at least ~50 ms apart". **The whole
acceptance rests on a human being slow.** An agent looping `update_task` has no such floor.
The race lives in the TUI runtime, which `tokio::spawn`s each write; the MCP crate can
decline to inherit it by awaiting each `store.queue()` in order. Free, but only if it is a
deliberate decision rather than an accident. BUG-2's entry should stop claiming no
reachable trigger once `tui-do mcp` exists.

**(b) `Store::write` opens deferred transactions, and should open immediate ones.**
`crates/tui-do-core/src/store/mod.rs:208` uses `guard.transaction()` — rusqlite's default,
which is `Deferred`: it takes a read snapshot and upgrades on first write. In WAL, a
transaction whose snapshot has been overtaken answers **`SQLITE_BUSY_SNAPSHOT`**, and
`busy_timeout` does **not** retry that one away — waiting would deadlock, so SQLite returns
at once and the transaction must be rolled back.

Invisible today because the in-process `Arc<Mutex<Connection>>` serialises every write.
Genuinely reachable the moment two processes write. And `Store::queue` is exactly the
hazardous shape: `next_local_id` reads `sync_state` and then writes it, inside the caller's
transaction.

The fix is one line and touches nothing else — `write()` is the only transaction site
outside migrations, and all fifteen callers take a `&Transaction<'_>` and do not care how
it began. Cost is a marginally longer lock hold, bounded because a pull applies **one page
per transaction** and the server caps a page at 50.

**(c) The migration loop reads its version outside the transaction.**
`store/schema.rs:214` reads `PRAGMA user_version` *before* the loop, then applies each
migration in its own deferred transaction. Two processes starting together against an
out-of-date store both decide to apply the same migration.

Checked rather than assumed: there are **zero** `IF NOT EXISTS` in that file (11
`CREATE TABLE`, 3 `ALTER TABLE`, 7 `CREATE INDEX`), so the loser fails **loudly** — and
each migration is one `execute_batch` in one transaction, so it rolls back whole. The
outcome is a confusing startup error, not a partial migration and not corruption. That is
the good version of this bug, and it is still worth closing: read the version inside the
same immediate transaction that applies the migration.

**(d) The lease.** Covered by the diagram and unchanged by any of the above. Worth stating
plainly so the two are not confused: **(b) stops two processes corrupting each other's
database writes; the lease stops two processes duplicating each other's server writes.**
Nothing about a transaction closes the `pending()` → network → `settle` gap, because that
gap contains a round trip.

**It is also a cost argument, and a bigger one than it looks** — four processes pull 312
pages every five minutes and three quarters of that is redundant, because they share the
store they are all filling. The measured numbers are under "The lease is a cost argument
before it is a correctness argument" below.

**(e) Lease churn, which argues the daemon is nearer than it looks.** An MCP stdio server
lives and dies with its client session. Three clients opening and closing all day means the
lease holder changes constantly and every ungraceful exit orphans the lease until it
expires — which pushes the duration *short*, against the requirement that it survive a
78-page full pull. A daemon outlives every client and the tension disappears.

**(f) Cross-process adoption has no path to a running TUI.** When a create is adopted
(`-3` → `812`), `SyncEvent::Adopted` retargets every in-memory holder — the model, the undo
stack, open modals. That is an **in-process** event. An adoption performed by an MCP process
never reaches the open TUI, which goes on holding a provisional id until its next reload.
Same family as the bug the label lifecycle fought hardest over, arriving from a direction
that did not exist when it was designed. Undesigned.

### The drain torture test — driven 2026-09-13

`md/drain-torture-test-plan.md`, with `md/2026-09-13-drain-torture-handoff.md` as the
prompt that precedes it. **Both are deliberately untracked** — `/md/*-plan.md` and
`/md/*-handoff.md` are gitignored, the same rule that keeps the manual checks local. They
live on `sw-x1`.

**It runs in two phases.** Phase zero upgrades `x1-omarchy` from **1.0.0** to **v1.0.2**
by building from git, which tests the README's build-from-source block on a box with no
checkout and an in-place upgrade over an existing binary — neither of which has ever been
driven. Checked rather than assumed: **no migration will run**, because the schema target
is v5 and that store is already at v5, so nobody should later read "upgrade tested" as
covering migrations. Build the **tag**, not `main`: 26 commits separate them and not one
touches `crates/`, so they are the same program today and would silently stop being so.

A bad Microsoft To Do import left **1,767 tasks in a project called `Archive`** with their
completion flag lost — `done = 0`, semantically finished years ago. Marking them done by
holding `d` is a thing the owner actually wants to do, and it is **the largest load this
project's write path has ever seen, by roughly 350x**. Dev is already seeded with the same
pile (project 28, 1,767 open, 64 done — identical to prod), so it can be driven against dev
with nothing to import.

It measures five things at once that are otherwise untested at scale: rule 1 under
sustained write pressure, whether the drain is O(n²) because `pending()` is re-read every
iteration, BUG-2's named residual (auto-repeat on `d`), `flush_on_exit` with a deep queue,
and (b) above under exactly the contention that triggers it.

**The plan names what it cannot see**, which is the part worth keeping: there is **no
tracing in the store layer at all**, so SQLite tail latency — the thing most worth knowing —
is not obtainable without adding code. That gap is itself a finding about the codebase.

**Run 1 was driven on 2026-09-13 and the pile is gone.** Full results, predictions judged
and all raw numbers are in `md/drain-torture-test-plan.md`. Run 2 (`flush_on_exit`) is
still outstanding. What matters to this file:

- **Rule 1 held.** 1,767 writes and 1,767 reloads, and the interface stayed responsive
  throughout with every keystroke instantaneous. That is the strongest evidence the
  architecture has, and it had never been tested above a handful.
- **The drain is O(n²), now measured rather than suspected:**
  `ms_per_entry = 0.0185 × depth + 133.8`. The `pending()` re-read costs **18.5 µs per
  queued row per iteration** — 33 ms/entry at the peak depth of 1,781, and **~11 % of the
  268-second run**. So the candidate-selection split `bugs.md` proposes under *Structure*
  is worth doing and is **not urgent**; it only dominates around 10,000 queued entries.
- **Correctness was perfect.** 1,767 distinct tasks written, all 3,732 requests `200`,
  server ends at 0 open / 1,831 done, zero retries, nothing ever deferred.
- **Two new defects, BUG-21 and BUG-22**, both in `bugs.md`. BUG-22 is BUG-2's named
  residual reproduced: 17 of 1,767 tasks (0.96 %) were pushed twice because a reload
  raced a write and re-showed a row the user had already marked. Harmless here only
  because both writes set `done = true` — `d` is a toggle, and the other resolution of
  the same race silently un-does the user's work.
- **Item 10(b) was not reached.** No `SQLITE_BUSY` at any point, from tui-do or from a
  second process reading the store once a second throughout. That is not evidence the
  hazard is absent — the sampler only ever read — but sustained write pressure from one
  process did not produce it.
- **The plan's own instrumentation had a bug worth remembering:**
  `TUI_DO_LOG=tui_do=trace` does **not** capture the per-request trace, which is emitted
  from `tui_do_api`. `EnvFilter` matches on module-path segments and `tui_do_api` is not a
  child of `tui_do`. All three crate targets are needed.

**Run 2 (`flush_on_exit`) was driven the same day and passes.** Quitting with 353 entries
queued is safe: tui-do declined to re-push, printed *"Still sending; leaving the rest
queued for next time."*, and the queue drained on the next launch as 353 POSTs for 353
distinct ids — zero duplicates, store and server agreeing exactly at 1,407 open / 424 done.

**It also found the most consequential defect of the exercise, BUG-23: the push is starved
by the UI's own writes.** Fifteen seconds passed with *no requests at all* while the key
was held, then four in 350 ms after release — roughly **7 % of the unloaded rate**. Run 1
shows the same shape in hindsight: its 6.3 entries/s was entirely post-release, and
essentially nothing drained during the 74-second hold.

**This changes the argument in (a) above, and it should be read before BUG-2 is dismissed
again.** BUG-2 is accepted on the grounds that its window is sub-50 µs and "the whole
acceptance rests on a human being slow". The unfairness it exploits — `std::sync::Mutex`
with no fairness guarantee, guarding the one `Connection` — turns out to have a second and
much larger consequence that a *human* reaches today, with no agent and no MCP server
involved: a held key stops the outbox draining. The mutex is now implicated in two
defects rather than one, which is a stronger case for fixing it structurally than either
makes alone.

### The test for (b), designed 2026-09-13

**Two `Store` handles on one file, not two processes.** SQLite locks per *connection*, not
per process, so two connections in one test reproduce the multi-process case exactly and
can be driven deterministically. `bugs.md`'s own rule applies — never test a race by
sampling it — so the interleaving is forced rather than hoped for:

```
  connection A                      connection B
  ------------                      ------------
  BEGIN (deferred)
  read sync_state      <- takes the read snapshot
  signal "read done"  ------------>
  sleep 250ms                       BEGIN; write sync_state; COMMIT
                                       (WAL advances past A's snapshot)
  write sync_state     <- SQLITE_BUSY_SNAPSHOT, deterministically
```

`Store::write` takes a caller-supplied closure, so the read, the signal and the write can
all live inside one closure with a channel — no new API and no test hook in production code.

**Before the fix:** A fails with `SQLITE_BUSY_SNAPSHOT` (code 517).
**After:** A holds the write lock from `BEGIN`, B blocks at its own `BEGIN IMMEDIATE` for
~250 ms, and both commit. `busy_timeout` is 5000 ms, so the margin is 20x — the sleep sets
the ordering, it is not a race window.

**One trap, found while designing it and worth writing down:** the obvious version has A
*wait for B to commit* rather than sleeping. That deadlocks after the fix — A holds the
write lock, B blocks at `BEGIN IMMEDIATE`, and A is waiting for a signal B can never send.
A generous sleep is what keeps the test honest in both directions.

**The same shape proves (c) for free:** stand up a store one version behind, let A read
`user_version` and B migrate and commit, then let A apply. It errors today.

### A documentation correction this turned up, true regardless of MCP

`CLAUDE.md` says, under "tui-do is multi-instance":

> the concurrency that matters is **concurrent writers against one server**, not two
> processes on one database file — there is no shared file, and nothing here needs
> cross-process locking.

**The fleet reasoning is right and the sentence overreaches.** `tui-do add` alongside a
running interface has been two processes on one file since Phase 4, whose design note says
so explicitly: *"Safe alongside a running TUI: the store is WAL with a 5s busy timeout, so
the two processes do not fight."* The two statements contradict each other, and the narrow
one is correct. MCP does not create this; it widens it from "two processes you start by
hand, seconds apart" to "four that launch when you open the laptop."

---

## Auth and credentials

**7. ~~Decide: OAuth first, or keychain first.~~ Decided 2026-09-08: keychain first.**
`tui-do login` is named and shaped now so the OAuth flow lands in it later rather than
displacing it. OAuth is blocked on item 1 merging *and* a server carrying it being deployed,
neither of which is ours to clear; the keychain is blocked on nothing. Recorded in
`md/2026-09-07-credential-storage-design.md`.

**8. Keychain credential storage.** *(raised 2026-09-07 — unblocked 2026-09-08)*
Designed, not built. Strictly additive: keychain → `TUI_DO_API_TOKEN` → `token_file` →
inline.

The musl worry turned out to be the wrong question, measured 2026-09-08: there is no
libsecret in this at all, and `keyring v3.6.3` offers three Linux backends that link three
ways. `async-secret-service` reaches the Secret Service through `zbus`, in pure Rust, and
links **no** C library — where `sync-secret-service` links `libdbus-1` and `libsystemd`, and
`linux-native` is the kernel keyring, which does not survive a reboot and is not what
"keychain" means to a user.

Two things left before code:

- **Confirm the static musl link in a container.** The workstation cannot: gnu target only,
  no `musl-gcc`, no `rustup`, and Docker's daemon is inactive with the user outside the
  `docker` group. Very likely fine — a backend linking no C library has nothing to fail to
  link — but "very likely" is what this note said about OAuth once.
- **Establish whether zbus's default features can be dropped.** `zbus v4.4.0` pulls
  `async-io`/`async-executor`/`blocking` even with keyring's `tokio` feature on, so the
  pure-Rust path currently means **two async runtimes** in the binary and 53 net-new crates
  against the present 412. That may change which backend is worth having.


**9. `tui-do login` — it does not exist, and it is the cheap half of the macOS goal.**
*(raised 2026-09-13)*
Verified rather than assumed: the binary has **three** subcommands — `add`, `migrate`,
`completions`. Item 8 and `md/2026-09-07-credential-storage-design.md` both talk about
`tui-do login` as though it were a place to put things. There is no such place yet.

**Build it for both platforms, decided 2026-09-13.** The first instinct was macOS-only,
since the goal that prompted it is a Mac one. Wrong instinct: the command's job is *writing
the config file for you*, and hand-editing YAML is no more pleasant on a Pi. Same command,
same prompts, same order; only where the token lands differs, which is exactly what the
platform seam in `CLAUDE.md` is for.

```
tui-do login
  ├─ "Server URL?"                    → writes config.yaml for you
  ├─ opens the browser at the server's API-token page
  ├─ you paste the token back (hidden input)
  └─ stores it:  macOS → Keychain    (item 8)
                 Linux → token_file, mode 0600, written right the first time
```

**What it is worth is the first-run experience, not the storage.** It deletes the `printf`,
the `chmod`, and the `$EDITOR config.yaml` — which is three of the six steps the README's
"REQUIRED — First time setup" currently asks for, on both platforms. It also gives the
README's honest "there is no first-run wizard" note something to become.

**And it is where OAuth lands later**, without displacing anything: the paste step
disappears and everything the user learned stays true. That was already the argument for
naming it now (item 7); this entry is only recording that naming it is all that has
happened.

---

## The macOS auth goal, and what it actually needs

*(raised 2026-09-13, from a discussion that is not otherwise written down)*

**The goal, in the owner's words:** hand-editing config files and `chmod` from the CLI is
not a Mac-like experience, and making the macOS auth experience Mac-like is the primary
goal of the port from here.

Three clarifications came out of that discussion and are worth keeping, because the notes
above mix them together and that is most of the confusion:

- **"Where the token lives" and "how you got the token" are different questions.** Keychain
  is storage. OAuth is sign-in. The Mac goal is entirely a *storage* problem plus item 9,
  and neither is blocked on the Vikunja spec PR that OAuth waits for.
- **No Apple Developer Program membership is required**, and no third party is involved.
  Keychain access is an ordinary OS API available to unsigned binaries. What the $99/year
  would buy is narrower: a stable code-signing identity, so the Keychain permission prompt
  is not re-asked after every upgrade, and Gatekeeper clearance for a browser-downloaded
  binary — which does not bite while the Homebrew formula builds from source.
  **Unmeasured:** whether the re-prompt actually happens on upgrade. Store an item, rebuild,
  read it back, see whether macOS asks. Cheap, and it is the only fact the money question
  turns on.
- **Split item 8 by platform.** macOS Keychain is always present and always unlocked at
  login, and costs a handful of crates. Linux's Secret Service is frequently *absent*
  (every headless box in the fleet) or *locked*, and costs 53 net-new crates plus a second
  async runtime. Building the macOS half alone delivers the whole stated goal and leaves
  the expensive, unmeasured Linux half unbuilt — possibly permanently, which would be a
  fine outcome.


## How this scales — the five tiers

*(worked out 2026-09-13; the frame items 3–6 and 10 sit inside, not a task of its own)*

The process model is in **`md/2026-09-08-mcp-process-model.png`**, tracked beside this file.
Read it first — everything below is that diagram extended outwards.

**The lease, in one paragraph.** Several agent processes share one SQLite store on one
machine. A *lease* — a row in `sync_state` naming a process id and an expiry — elects
exactly one of them to be the only one allowed to talk to Vikunja. It is **not** protecting
the database; SQLite already does that with `journal_mode = WAL` and `busy_timeout = 5000`.
It protects the *server*, because the damage it prevents happens on the wire, in the gap
between reading an outbox entry and marking it sent — a gap no database lock covers. Without
it, four processes read the same pending `CreateTask` and four identical tasks land in
Vikunja.

**tui-do already has this mechanism in the wrong place.** `RUNNING`, an `AtomicBool`, stops
two sync passes overlapping *inside one process*. It works perfectly and is invisible to a
second process, because it lives in memory. The lease is that flag moved into the database
where others can see it — a new row, not a new table.

```
  TIER 1  one person, several machines                     WORKS TODAY
  ----------------------------------------
     sw-x1       [store] --+
     x1-omarchy  [store] --+-->  Vikunja  <-- the only shared thing
     sw-pi       [store] --+
     Each machine keeps its own cache. r / R reconciles.

  TIER 2  one person, several agents, one machine          THE ONLY NEW WORK
  -----------------------------------------------
     agent x4  -->  [ one store + LEASE ]  -->  Vikunja
     The lease, and nothing else. Note the open interface is a writer too.

  TIER 3  one person, agents on several machines           FREE
  ----------------------------------------------
     sw-x1       [store + lease] --+
     x1-omarchy  [store + lease] --+-->  Vikunja
     A lease belongs to a FILE, so each machine's is independent. They
     reconcile at the server exactly as two people would. Nothing to build.

  TIER 4  a team                                           NEEDS BOT USERS
  --------------
     Steve  [store + lease] + agents --+
     Alice  [store + lease] + agents --+-->  Vikunja
     Bob    [store + lease] + agents --+
     Architecturally identical to Tier 3, repeated per person.

  TIER 5  enterprise, several teams                        THREE THINGS BREAK
  ---------------------------------
     The same shape again, N times over. See below.
```

**Tier 4's blocker is attribution, not concurrency.** The concurrency was solved in Phase 2
— `Task::merge_onto` already assumes several machines writing to one server. What fails is
*identity*: if an agent acts as its owner, the board says **Steve** filed the task and
nobody in review can tell agent work from human work.

### Identity decides everything else

The two options are not independent of the diagram, because **the store caches what one
user can see**:

```
  OPTION 1 - agents act as YOU         OPTION 2 - each agent is a bot user
  ----------------------------         ----------------------------------
  one Vikunja user                     many Vikunja users
  one API token                        many tokens
  ONE store  <-- shareable!            MANY stores <-- nothing to share
  agents share it -> lease works       each agent syncs alone
  cheap: one sync loop, one cache      N x sync loops, N x 3,877 tasks
                                       on disk, N x server load
```

One identity is what buys the shared store; the shared store is what makes the lease worth
having. Change the identity model and the diagram comes apart. For a single-user homelab,
option 1 is plainly right — which is what `md/2026-09-08-mcp-server-design.md` §9 already
chose, with `AgentIdentity::Bot` written down beside it as the honest, more expensive route.

### The lease is a cost argument before it is a correctness argument

**Measured against the code on 2026-09-13, not estimated.** `spawn_sync_timer`
(`runtime/mod.rs:695`) fires `Pass::Full` every interval, and `Sync::once` — what startup
calls — is `pass(Reach::Full, ..)`. So **every process pulls all 78 pages every five
minutes**, plus another 78 at startup. Not a cheap delta: the whole listing, ~3,877 tasks,
about 15 seconds — **measured at 25.0 s on 2026-09-13**, 78 pages and 83 requests against
a cold store, so the numbers below understate the cost by two thirds.

That is deliberate and should not be "fixed" by shrinking it. **Only a full pull may
delete** — a filtered listing cannot tell "unchanged" from "deleted elsewhere" — and
`CLAUDE.md` records the decision of 2026-08-28: *"Startup and the timer stay full."*

**What that costs at the configuration this is actually for** — three MCP connectors plus
an open interface:

```
   4 processes x 78 pages = 312 requests every 5 minutes
   4 processes x 25s      = 100s of server work per 300s window  (measured)

   |####################|........................................|
   0min               1min                                     5min
   #### Vikunja busy      .... idle

   Busy one minute in every five, re-sending data it already sent.
   Opening the laptop fires all 312 at once.
```

**And at Tier 4/5**, 10 people x 5 agents = 50 processes:

```
   50 x 78 = 3,900 requests / 5 min  ~= 13 req/sec sustained
   50 x 25s = 1250s of work per 300s window  (measured)

   The polling alone exceeds capacity. The server never catches up,
   before anybody does any actual work.
```

**The local file is loaded too, and in the worst possible way.** Each of those pulls writes
~3,877 upserts into the *same* SQLite file, in page-sized transactions. N processes x 78
write transactions per interval, all contending for one write lock — which is precisely
when (b) above loses its snapshot. The two problems feed each other: more pulling means
more writing means more `SQLITE_BUSY_SNAPSHOT`.

**The point that makes this decisive: with a shared store, N-1 of those pulls are pure
waste.** All N processes fetch the same rows into the same file. One pull already serves
everyone.

```
   WITHOUT THE LEASE             WITH THE LEASE

   [proc] 78 pages --+           [proc] 78 pages --+
   [proc] 78 pages --+           [proc]            |
   [proc] 78 pages --+--> DB     [proc]            +--> DB
   [proc] 78 pages --+           [proc]            |
                                                   +
   312 requests                  78 requests
   4 copies of the same data     one copy, shared
```

So the lease is not only a correctness fix for duplicate writes. **It removes three
quarters of the traffic at four processes**, and it does so without giving up the
deletion-detection that makes the pull expensive in the first place. That argument holds at
Tier 2, not only at scale.

### Making the pull cheaper — four leads, two of them easy

*(raised 2026-09-13, from asking why 10-year-old archived tasks are re-read every
five minutes. They are not — see the correction first.)*

**The premise correction, because it changes what to optimise.** Archived projects' tasks
are **not in the listing at all**: measured in BUG-15, an unfiltered `GET /tasks` returned
3,879 tasks across 78 pages and **none** of the four belonging to an archived project. They
cost nothing per pull because they are never fetched — and by the same token a store that
lost them never gets them back, and a fresh install never sees them. What actually fills the
listing is **done tasks: 1,942 of 3,877, half the payload.**

**The rule that rules out the obvious fix.** A filtered listing can never be swept: "absent"
and "did not match the filter" are indistinguishable, which is exactly why
`Reach::Incremental` exists and is forbidden from deleting. Filtering the full pull
reintroduces BUG-15 by hand. **So the lever is frequency, not content.**

**Lead 1 — pull full rarely, incremental often.** *(easy, no new anything)*
The timer runs `Pass::Full` every interval. Run `Incremental` on the timer and `Full` on a
much longer schedule (startup, plus hourly or daily), and the cost drops ~99% — 288 full
sweeps a day becomes one. The honest cost is that a task deleted elsewhere can take up to
that interval to disappear rather than up to five minutes; `R` still forces it. That is a
scheduling change, not a protocol one, and it does not weaken deletion detection — it
reschedules it.

**Lead 2 — stop rewriting rows that did not change.** *(easy, and separate from lead 1)*
`upsert_task` (`store/tasks.rs:448`) is an unconditional
`INSERT ... ON CONFLICT (id) DO UPDATE SET` over **every column**. Nothing compares
`updated` first, so all ~3,877 rows are rewritten on every pull whether or not a character
changed — roughly 1.1 million row writes a day against an idle server. A
`WHERE excluded.updated > tasks.updated` guard skips nearly all of it.

This is the *local* half of the cost and it compounds (b) above: every one of those writes
is contention on the one write lock, which is precisely when a deferred transaction loses
its snapshot. Lead 1 cuts the network cost; lead 2 cuts the disk cost; they are independent.

**Lead 3 — webhooks, unmeasured.** Vikunja has them and nothing here had noticed:
`GET /webhooks/events`, `PUT /projects/{id}/webhooks`, and an account-level
`/user/settings/webhooks`. If a `task.deleted` event exists, deletions could arrive as they
happen instead of being inferred from a sweep.

**The first experiment is one request:** `GET /webhooks/events` against dev, and read the
list. The spec does not enumerate the event names — which is exactly the shape of thing this
project has already got wrong once by reading a document instead of asking the server.

Two caveats that keep it from being a silver bullet, both worth knowing before spending time
on it. A webhook needs somewhere to arrive, and a laptop has no address — less absurd on the
tailnet, where Serve already fronts Vikunja, but a deployment change rather than a code
change. And **push is an optimisation, reconciliation is the guarantee**: a laptop asleep or
offline misses events silently, so a periodic full sweep stays necessary whatever webhooks
do. They make it rare; they do not remove it.

**Lead 4 — a scoped sweep, for completeness.** Fetch `done = false` (≈39 pages rather than
78) and sweep only against local tasks that are also `done = false`. The subtlety that makes
it a design job: a task marked done *elsewhere* also vanishes from that listing and would be
wrongly deleted, so it needs a second cheap lookup to separate "deleted" from "now done".
Possible, not worth doing before leads 1 and 2.

### What actually breaks at Tier 5

1. **Labels are one global pool.** Item 3 stores agent state as `agent:todo`,
   `agent:in-progress`, `agent:in-review`, `agent:scrapped`. Vikunja shares its label pool
   across *every project and every user on the instance*. Fine for one person; at fifty
   teams it is one namespace with no way to scope a workflow. **This is a second and
   stronger argument for item 4** — a bucket belongs to a view, and a view to a project, so
   buckets scope correctly where labels cannot.
2. **Token provisioning.** Hand-minting a token per person and per bot, ticking seven
   permission boxes each time, does not survive fifty people. That is where OAuth stops
   being a convenience and becomes the provisioning mechanism — a different argument for
   item 1 than the one recorded there.
3. **Attribution.** Bot users, per above, at a store and a sync loop each.

### The principle, and the honest limit

**tui-do scales by staying per-person.** Vikunja is the thing that scales to a team and it
already does that job. The temptation at Tier 4 will be to make one tui-do store serve
several people; that is rebuilding Vikunja, badly, on SQLite.

**SQLite shared across processes is a one-machine trick.** Fine for a handful of agents on
one laptop, not a server architecture. The `localhost:7777` daemon in the diagram's
bottom-right is the natural ceiling — one machine, one owner, many agents, one process
holding the store and the network — and at that point the lease is unnecessary because
there is one writer by construction. Item 10(e) argues that ceiling arrives sooner than the
diagram implies.

### Still open

- **How long is the lease?** Too short and a full pull (78 pages, **25s measured** against dev) loses it
  mid-sync; too long and a crashed agent blocks the network for that long. The diagram says
  `expires 12:04:30` without naming the interval. Choose it deliberately.
- **An agent that holds the lease and dies mid-send.** The expiry frees the lease, but the
  entry it was sending may or may not have reached Vikunja. That is BUG-19's shape exactly,
  and `Sync::create_or_adopt` exists to handle it — confirm it covers this path rather than
  assuming.

---

## Launch and outreach

**Decided 2026-09-13: spend nothing that can only be spent once.** "Another TUI to-do
client" is a crowded story, made worse by the `gouveags/tui-do` name collision. "An agent
files its own tasks into a self-hosted server and the terminal client picks them up" is a
different story, and it needs item 3 shipped. So the one-shot channels are held.

**Held for the MCP launch (item 3):** Show HN, `r/selfhosted`, a `community.vikunja.io`
follow-up, `r/rust`. Spend them together, frame on the agent rather than the TUI, and
pre-empt the name collision in one line — the analysis is in `CLAUDE.md` and it costs a
sentence to own it and a derailed thread not to.

Lead into the README's data-loss warning and the public `bugs.md` rather than softening
them. On those audiences an open defect list reads as credibility.

**Done 2026-09-13:** GitHub topics, homepage, Discussions with a pinned welcome post, and
the social preview card (`docs/demo/social-preview.png`, uploaded by hand — GitHub exposes no
API for it).

**Blocked until 2027-02-24: `rothgar/awesome-tuis`.**
[#890](https://github.com/rothgar/awesome-tuis/pull/890) was **auto-closed by their bot**,
not reviewed by a person: the list requires a repository to be at least **six months old by
first commit**, and tui-do's first commit is `603a331`, 2026-08-24. The branch and the fork
are still there, and the bot says the status table refreshes on reopen or push — so this is
`gh pr reopen 890 --repo rothgar/awesome-tuis` on or after **2027-02-24**, not a rewrite.

Worth knowing before submitting anywhere else: **a minimum age bar is a category of rejection
nothing in the repository can fix.** Check for one before spending the effort, the way
`awesome-selfhosted`'s scope was checked rather than attempted.

**Ruled out:** `awesome-selfhosted`. Its scope is "network services and web applications
which can be hosted on your own server(s)" — tui-do is a client, not a hostable service, so
it does not qualify. Checked rather than attempted.

**Weaker than assumed:** This Week in Rust. Crate of the Week nominations are taken on the
users.rust-lang.org forum rather than on GitHub, and self-nomination is discouraged there.
It needs someone else to nominate, or a blog post to submit instead.

**Left to do by hand:** Terminal Trove (web form, no submission repo exists), and a
Mastodon post.

**The referrers say where the value is**, measured 2026-09-13 and worth re-reading before
the next push: `community.vikunja.io` 40 views / 5 uniques, `github.com` 17/6,
`vikunja.io`'s integrations page 3/3. The forum post did nearly all the work and the
integrations listing sends almost nothing — which is the argument for saving the forum
follow-up for when there is something to say.

Baseline to measure against: **7 stars, 104 views / 27 uniques over 14 days.**

```sh
gh api repos/sjwasko/tui-do/traffic/popular/referrers   # which channel actually worked
```

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

- The three structural troubleshooting surfaces in `bugs.md` (`sync::push_with`,
  `runtime::add`, `update::apply_edit`) are still the worst places to debug. **`runtime::add`
  is scheduled**: item 3's branch splits it into `resolve_or_explain` and `report` exactly as
  `bugs.md` §2 prescribes, because an MCP server cannot call a function that writes to the
  stdout it speaks JSON-RPC over. The other two are untouched.

---

## Done

- **2026-09-13** — **macOS is driven, and the README's platform claim is backed.** Rendering
  including tmux, keybindings, sync from inside the interface, sleep/wake, and `o`'s copy
  branch over SSH, across Terminal.app and iTerm2. Procedure and results in
  `md/2026-09-08-macos-test-plan.md`; `md/2026-09-07-macos-port.md` §5 no longer sets the
  limit of the port.

  Worth keeping: **OSC 52 works in Terminal.app**, where that plan's §4 predicted it would
  not and said the finding would become a README caveat. It needs none. A prediction written
  down and then measured is the only reason anyone can tell the difference between a caveat
  that is true and one that was inherited from folklore.

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
