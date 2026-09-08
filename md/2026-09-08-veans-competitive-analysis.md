# veans and tui-do: what overlaps, what doesn't, and where they compose

Written 2026-09-08, after kolaente replied to the announcement thread on
`community.vikunja.io/t/.../5076` with two pointers: send a PR to the External Integrations
page, and *"on the agent things, check out veans."*

Sources: `vikunja.io/docs/veans/` read in full, and
`github.com/go-vikunja/website/blob/main/src/content/docs/integrations/integrations.mdx`
read at source. Nothing here is inferred from the summary of either.

**The conclusion first: veans is not a competing TUI and tui-do is not a competing agent
CLI. They occupy the two halves of one workflow, and veans's own documentation hands its
missing half to the web browser** — which is the exact flow-break the tui-do README opens
by complaining about.

---

## 1. What veans is

> "a small, experimental command-line tool for driving Vikunja from a terminal, a script,
> or a coding agent. It wraps Vikunja's REST API with an agent-friendly surface and can
> emit a system prompt that teaches an agent to track its work in Vikunja instead of a
> scratch to-do list."

Three facts about it that matter more than the feature list:

- **It ships with the server.** "Built and published alongside the main Vikunja binary,"
  and installable from Vikunja's own package repositories on Debian/Ubuntu, Fedora/RHEL,
  Arch and Alpine. That is distribution tui-do cannot match and should not try to.
- **It is built for machines, explicitly.** "veans itself is built for machines, not
  people. Everything prints JSON… **The human side of the workflow happens in Vikunja.**"
- **It needs Vikunja ≥ 2.4.0** and creates a dedicated bot user, signing in by OAuth
  2.0 + PKCE against Vikunja's built-in authorization server ("2.3+").

The workflow it imposes is five fixed Kanban buckets — Todo → In Progress → In Review →
Done, plus Scrapped — created by `veans init`. One rule holds it together: **the agent
never closes its own task.** It parks work in In Review and a human signs it off.

That rule is good product design and worth stealing outright as a concept, whatever else
is decided here.

### The verb surface

| verb | what it does |
|---|---|
| `init` | sign in, create the bot, mint a token, write `.veans.yml` |
| `prime` | print the agent system prompt; silent with no `.veans.yml` |
| `list` | list tasks (`--ready --mine --branch --filter --status`); JSON |
| `show <id>` | one task, JSON |
| `create "title"` | `--description --label --status --priority --parent --blocked-by` |
| `update <id>` | `--status --title --priority --label-add/remove --comment …` |
| `claim <id>` | assign the bot, move to In Progress, tag with the current git branch |
| `api METHOD PATH` | raw REST escape hatch |
| `login` | re-mint the bot's token |
| `version` | |

Task ids accept `PROJ-NN`, `#NN`, or a bare number.

---

## 2. Invocation model: flags versus quick-add

The tempting claim is that tui-do's fuzzy parser beats veans's flags. **Half of that is
right and the half that is wrong is worth knowing before it gets said in public.**

```
veans   create "Chase the upstream ticket" --priority 4 --label urgent --status todo
tui-do  add 'Chase the upstream ticket *urgent !4 +Infra tomorrow'
```

### Where quick-add genuinely wins

1. **One grammar, two audiences — and this is the strongest form of the argument.** The
   string the agent emits is the string the human types into the add box. veans has two
   vocabularies: flags for the agent, the web UI for the person. tui-do's agent surface
   *is* its human surface, so there is one thing to learn, one thing to document, and one
   parser to keep correct.
2. **It is not a new API.** Quick-add magic is *Vikunja's own documented syntax*
   (`crates/tui-do-core/src/quickadd/mod.rs` was written from that documentation, not
   ported from cria). A model that knows Vikunja already knows it. veans's flag set is a
   surface that must be taught — which is precisely why `veans prime` exists and has to be
   re-emitted on every session start and every context compaction.
3. **It degrades instead of failing.** `--prioirty 4` is a hard error the agent must
   recover from. `!4` mistyped as `!!4` still files the task; the token lands in the title
   as text. For a long unattended run, partial success beats a non-zero exit.
4. **Token order is irrelevant** — no positional grammar to get wrong.

### Where flags genuinely win, and pretending otherwise would be a mistake

1. **Ambiguity is a real cost.** A task legitimately titled `Fix the *important* bug` or
   `Ship v2 !now` is a landmine for a fuzzy parser and is nothing at all to a flag. Point
   3 above cuts both ways: a token that silently becomes title text is a *silent* partial
   failure, and nobody notices the label that never got applied.
2. **JSON out.** veans prints machine-readable output from every command. `tui-do add`
   prints `Sent.` or `Queued.` This is the larger gap and it is on the **read** side.
3. **veans can read; tui-do cannot.** `list --ready --mine --branch --filter` and `show`
   have no counterpart. The README says so plainly: *"Today `add` is the whole agent
   surface."*

**So the competitive gap is not syntax. It is verbs and output format.** tui-do's write
ergonomics are better and its read surface does not exist. Arguing syntax superiority
while missing `list` is arguing the wrong point in a conversation tui-do would lose.

---

## 3. What veans structurally cannot do

Not criticism — these follow from it being a thin REST wrapper, which is a deliberate and
correct choice for what it is.

- **No offline.** Every command is a network round trip. Server down, VPN dropped, on a
  plane: veans fails. tui-do queues in the outbox and reconciles later. For an agent on a
  long unattended job this is not hypothetical.
- **No local cache**, so every `list` pays the round trip and lives with the server's
  pagination cap.
- **No human interface**, by its own statement. The person watches the board move in the
  browser.
- **A fixed five-bucket model.** It expects the buckets it created and maps its statuses
  onto them. An existing board with a different shape does not fit; the escape hatch is
  raw `veans api`.
- **It needs 2.4.0+, a bot user, and an authorization server.**

---

## 4. Better together: the actual composition

veans owns the **agent's hands** — workflow discipline, bot identity, per-repo config,
session priming. tui-do owns the **human's seat**, in the terminal, offline, instant.

veans's documentation describes a loop with a hole in it: *watch a task move across the
board in real time, comment on it, reprioritize it, pick up where the agent left off* —
all of which it expects to happen in a web browser, in another window, in another
application. **tui-do fills that hole without changing anything about how veans works,**
because both are just writing to the same Vikunja project.

The demonstrable version is one tmux window: the agent working in the left pane, driving
veans; tui-do in the right pane, showing the task move Todo → In Progress → In Review as
it happens; the human pressing a key to sign it off. Neither tool needs to know the other
exists.

### What tui-do would have to build for that, in order

1. **Kanban / bucket awareness.** This is the blocker and it is unglamorous: tui-do today
   cannot display the thing veans moves. Kanban is already on the "Ahead" list; this
   promotes it from "Vikunja parity" to "the feature that makes the agent story real."
2. **Read verbs with `--json`.** `tui-do list`, `tui-do show`. Reaches parity with veans
   as an agent surface *and* — the part veans cannot copy — answers **from the local
   store**, so it is instant and works offline. An agent that only needs to read its own
   task list never touches the network.
3. **A review queue.** A view filtered to In Review, which is where veans's one rule
   deposits everything and where the human's sign-off has no home today. This is a feature
   veans structurally cannot build, and it is the highest-value small thing on this list.
4. **An agent inbox — a project or label that is the agent's feed.** Specs, tasks and
   context filed to it by a human from `tui-do add`, picked up by the agent. This is the
   "setting up a project or label associated with an agentic project" idea and it needs
   nothing new on the wire: it is a saved filter plus a convention.

### What not to do

**Do not build a competing agent CLI.** veans installs from `apt`, ships in the server
binary, and is documented on vikunja.io. tui-do would be fighting distribution it cannot
win, on the one axis where the incumbent is strongest, while its own differentiator —
offline, local-first, a real interface — goes unused.

---

## 5. A correction this turned up — now settled

`md/2026-09-07-credential-storage-design.md` concluded that **OAuth is not available** on
our Vikunja and deferred the question to a server upgrade. **Probed on dev the same day
this was written: that conclusion is wrong. The authorization server is live on `v2.5.0`.**

`OPTIONS /api/v1/oauth/authorize` and `/api/v1/oauth/token` both answer
`Allow: OPTIONS, POST`; `POST /api/v1/oauth/token` with an empty body answers `400` code
`17007` naming `authorization_code` and `refresh_token` as the supported grants. The same
prefix with a nonsense suffix answers `404`, so the `405` the original note filed under
"not available" was the route existing, not missing.

The full correction, including the two measurement mistakes that produced the original
answer, is in that note. The short version is that **an SPA serves identical HTML for every
route it owns**, so comparing a body against a nonsense path proves client-side routing and
nothing else — and the probe never asked the API.

**veans's documentation was right and ours was wrong**, which is worth saying plainly given
this file spends its length comparing the two.

**The keychain half is independently confirmed.** veans stores the bot token in the OS
keychain, falls back to `VEANS_TOKEN`, then `~/.config/veans/credentials.yml` at `0600` —
the same three sources in the same order the design note reached on its own.

**One blocker that is ours, not the server's:** the OpenAPI document has no `oauth` path —
126 paths, checked-in and re-fetched live, neither contains one. Rule 3 says no endpoint is
called that isn't in `spec/vikunja.json`, and a conformance test asserts it. Using OAuth
means an argued exception or an upstream spec fix, decided before anyone starts.

---

## 6. The External Integrations PR

- **Repo:** `github.com/go-vikunja/website`
- **File:** `src/content/docs/integrations/integrations.mdx`
- **The page invites it:** *"Feel free to contribute to this page."*

Entries are `## Name`, one descriptive paragraph opening with a link, then a
`Visit the … to learn more.` line. Six today: vja, vja-review, tw2vikunja, Home Assistant,
Cria, mDone. Append rather than insert.

**The page is labelled "community-maintained tools and integrations."** That is a
different shelf from `/docs/veans/`, which sits under API & Integrations as first-party.
Worth knowing accurately: the invitation is real and it is the right first step, but it is
the shelf cria sits on, not the one veans does. Being the only *maintained* Vikunja TUI on
that page is the thing that changes it.
