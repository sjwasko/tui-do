---
name: tui-do
description: Create tasks in Vikunja from the shell with `tui-do add`, using quick-add syntax — labels, priorities, projects, due dates and repeats in one line. Use when an agent needs to record work, file a follow-up, or queue a task for a human, on a machine where tui-do is installed and configured.
---

# Adding tasks with tui-do

`tui-do add` writes a task to the local store immediately and queues it for the Vikunja
server. One line in, no JSON, no ids to look up.

```sh
tui-do add "Chase the Telnyx DID *urgent !4 +Infra tomorrow"
```

That single call resolves a label, a priority, a project and a due date. Constructing the
equivalent Vikunja request means knowing a project id, a label id, and that "unset" on the
wire is Go's zero time rather than `null`.

## Why this is the right surface

**It never blocks and it never loses the task.** The write lands in a local SQLite store
first and goes to the server after, so `tui-do add` returns at once whether or not the
server is reachable. If it is not, the task is queued and the next run sends it.

**It is the same path the interface uses** — the same outbox, the same retry with backoff,
and the same three-way merge that replays your change onto the server's current copy rather
than overwriting whatever arrived in between. An agent that calls the Vikunja API directly
is a second writer with none of that.

## The tokens

Tokens may appear anywhere in the line and are removed from the title.

| token | meaning |
|---|---|
| `+project` | file it in a project, by title or id: `+Legal`, `+#12` |
| `*label` | attach a label that already exists: `*urgent` |
| `@user` | assign someone: `@admin` |
| `!1` … `!5` | priority, 1 lowest to 5 highest |
| a date | `tomorrow`, `next friday`, `27/08/26`, `27aug26`, `2026-08-27` |
| `due <date>` / `start <date>` | the same, said explicitly |
| `every <n> <unit>` | repeat: `every 2 weeks`, `every month` |

**Names with spaces go in brackets:** `tui-do add "+[Dinner Places] Book a table"`.
Brackets beat quotes from a shell, because the shell strips quotes before tui-do sees them.

## Flags that matter when nobody is watching

| flag | when to use it |
|---|---|
| `--offline` | queue without attempting to send; use when you know there is no network and do not want the attempt |
| `--create-labels` | create any label the line names that does not exist yet |
| `--config <PATH>` | use a specific config, rather than the default lookup |

**`--create-labels` is off by default and should usually stay off.** Labels are one global
pool shared by every project, so a typo in an agent-generated line becomes a permanent entry
that pollutes completion everywhere. The interface can ask a human; an unattended command
cannot, so it takes an instruction instead. Pass it only when the label vocabulary is
controlled by you rather than by a model.

## Checking it worked

`tui-do add` exits non-zero and explains itself if the line cannot be parsed or the config
is unusable. A zero exit means the task is in the local store — **not** that the server has
it yet. That is the design, not a limitation: the queue drains on the next run.

<!--
  POST-GA, NOT BUILT YET. Kept here so this file is the one place the agent surface is
  described, and so the shape can be argued with before it is written. Do not document
  these as though they work; they do not exist in the GA binary.

  Reading:
      tui-do ls --filter 'done = false' --json
    Answers from the local store, so it is instant and works offline. The --json contract
    is undecided: once something depends on the shape, it is an API.

  Completing:
      tui-do done <id>
    Queues the same UpdateTask the `d` key does, through the same outbox.

  Moving a task across a Kanban board:
      tui-do move <id> --view <view> --bucket <bucket>
    So a multi-agent run can be watched as cards moving through columns in Vikunja's own
    web UI, with no dashboard to build. Note a bucket belongs to a *view*, not a project.

  Rationale and open questions for all three are in PLAN.md, under
  "Post-GA — the CLI as the agent surface" and "Post-GA — a Kanban board as the run
  surface for a multi-agent process".
-->

## Want more than `add`?

Reading, completing and moving tasks from the command line are planned but **not in this
release** — today `add` is the whole agent surface. If you want to drive more of tui-do from
an agent, **please open a feature request** at
<https://github.com/sjwasko/tui-do/issues> and say what you were trying to automate. Which
verbs get built first is being decided by what people actually ask for.
