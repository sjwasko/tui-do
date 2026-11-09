---
name: tui-do
description: Create tasks in Vikunja from the shell with `tui-do add`, using quick-add syntax — labels, priorities, projects, due dates and repeats in one line. Use when an agent needs to record work, file a follow-up, or queue a task for a human, on a machine where tui-do is installed and configured.
---

# Adding tasks with tui-do

`tui-do add` writes a task to the local store immediately and queues it for the Vikunja
server. One line in, no JSON, no ids to look up.

```sh
tui-do add 'Chase the upstream ticket *urgent !4 +Infra tomorrow'
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

Tokens may appear anywhere in the line. Labels, projects, users, priorities and repeats
leave the title wherever they are. A date is always read, but leaves the title only when
nothing follows it except whitespace, punctuation and other tokens; anywhere else it stays
exactly as typed. So `call mom tomorrow` is titled `call mom`, while `tomorrow buy milk`
keeps its title and is still due tomorrow: put the date last when it should not be part of
the title.

| token | meaning |
|---|---|
| `+project` | file it in a project, by title or id: `+Legal`, `+#12` |
| `*label` | attach a label that already exists: `*urgent` |
| `@user` | assign someone: `@admin` |
| `!1` … `!5` | priority, 1 lowest to 5 highest |
| a date | `tomorrow`, `next friday`, `27/08/26`, `27aug26`, `2026-08-27` |
| `due <date>` / `start <date>` | the same, said explicitly |
| `every <n> <unit>` | repeat: `every 2 weeks`, `every month` |

## Learn the names once with `--tokens`

Before generating lines, run `tui-do add --tokens` once. It lists the projects, labels and
users this machine's store knows, one per line and grouped, each written exactly as you
should type it — `+[Life Admin]` for a project with a space — then the token syntax. It
reads the local store only: it adds nothing, writes nothing and contacts no server. With no
store yet it exits non-zero and says to run `tui-do` once to sync. Plain text only; there is
no `--json` yet.

```console
$ tui-do add --tokens
projects:
+Legal
+[Life Admin]

labels:
*[needs review]
*urgent

users:
@admin

syntax:
  +project   +Legal, +[Home Improvement], +#12
  ...
```

Use only names from that list, then check each line with `--dry-run`.

## Check the phrasing with `--dry-run`

Run the line with `--dry-run` before adding it whenever it was generated rather than
copied. It resolves the project and labels against the local store exactly as a real add
would and prints what it would create, then queues nothing, writes nothing and contacts no
server. A line that would fail exits non-zero with the same message.

```console
$ tui-do add --dry-run 'Call the dentist tomorrow +Personal *health !3 every week'
Would add "Call the dentist" to Personal, due Mon 9 Nov 2026
Parsed: title "Call the dentist" · +Personal · *health · !3 · due Mon 9 Nov 2026 · every week
as typed best: Call the dentist +Personal *health !3 due 2026-11-09 every week
Dry run: nothing was queued or sent.
```

It checks projects against the local store only. A project created elsewhere since the
last sync can fail a dry run and still succeed for real, because a real add asks the server
once before giving up; if a dry run says a project you know exists is missing, run `tui-do`
once to sync.

**Write lines the way `as typed best:` shows, and copy that form.** The title, then
`+project`, `*label`s, `@user`s and `!N`, then each date after its keyword in ISO form
(`due 2026-11-09`, `start 2026-11-09 at 09:00`), then the repeat. Dates go last because a
date leaves the title only when nothing follows it except whitespace, punctuation and other
tokens. A real add prints the same `as typed best:` line under its `Added` line whenever what
was typed differs from it; when it is silent, the line was already in its best form.

**Quote with single quotes.** At an interactive bash prompt `!3` is history expansion,
including inside double quotes, so the priority is eaten before tui-do sees it. Scripts and
agents run non-interactively, where this is off, but single quotes are correct everywhere.

**Names with spaces go in brackets:** `tui-do add '+[Dinner Places] Book a table'`.
Brackets beat quotes from a shell, because the shell strips quotes before tui-do sees them.

**Always pass the text.** `tui-do add` with no text opens an interactive prompt with Tab
completion and `?` help, but only at a terminal, and it is for people. An agent passes the
line as an argument, and checks one it is unsure of with `--dry-run` first. Run with no
text and no terminal, `tui-do add` is a usage error, exit code 2.

## Adding a description

Put the context — a stack trace, a spec, what you tried — in the description, not the
title. **Use `--description-file -` with a heredoc whose delimiter is quoted**, so nothing
in the body is expanded by the shell (`$VAR`, backticks and `!` all arrive as typed):

```sh
tui-do add 'Fix the flaky sync test +Infra !3' --description-file - <<'EOF'
## What fails

`a_pull_does_not_delete_a_task_created_offline` fails about 1 run in 20.

## What I tried

1. Re-running with `--test-threads=1`: still fails.
EOF
```

- The title line is quick-add; **the description is not**. `+project`, `*label` or a date
  in the body stays text, so keep every token on the title line.
- It is stored exactly as given — Markdown is fine — but for one trailing newline. It must be
  UTF-8 and at most 64 KiB; anything else exits non-zero and queues nothing.
- With `-`, the task's text must be an argument: stdin is the description.
- `-d 'text'` is fine for a single short line. Do not use `--edit`; it opens an editor and
  refuses to run without a terminal.

The confirmation counts the description and never prints it:

```console
Added "Fix the flaky sync test" to Infra, with a description (4 paragraphs, 157 characters)
```

`--dry-run` takes the same flags and adds a `Description begins: "…"` line, so you can check
the right text was read.

## Flags that matter when nobody is watching

| flag | when to use it |
|---|---|
| `--tokens` | list the projects, labels and users a line may name, and the syntax; run once first |
| `--dry-run` | print what the line would add and its best form, and add nothing |
| `--description-file -` | read the task's description from stdin; see above |
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
