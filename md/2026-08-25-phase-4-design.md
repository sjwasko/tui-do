# criax — Phase 4 design: mutations

Agreed 2026-08-25, before any code. `PLAN.md` remains the authority on scope; this records
the decisions and why, in the same spirit as `md/2026-08-24-1900-tui-design.md`.

## The four decisions

| | chosen | over |
|---|---|---|
| Destructive actions | **No confirms.** Everything is instant and `u` undoes it | confirming deletes; confirming anything destructive |
| Editing | **Quick keys for the common fields, `e` for a form over the rest** | inline in the preview; a full-screen editor |
| Quick-add | **A status-line prompt, parsed live** — *and* a `criax add 'text'` subcommand | a prompt with no feedback; a modal |
| Undo | **Unlimited within a session**, cleared on exit | persistent across restarts; single level |

## Why no confirms

Undo is not a consolation prize here, it is the better mechanism. A confirmation dialog
asks the user to predict a mistake; undo lets them recognise one. The outbox already
carries `Mutation::inverse`, so undo costs nothing new — it queues the inverse like any
other change, which is rule 5 of `CLAUDE.md` rather than a parallel path.

The one honest caveat, and it goes in the toast: **Vikunja has no undelete.** Undoing a
delete re-creates the task, so it comes back with a new id, and anything that referenced
the old one — a relation, a link someone pasted in a comment — does not come back with it.

## The write path

`Store::queue` already applies a mutation locally *and* queues it for the server in one
transaction, so there is exactly one write effect:

```
key ─▶ update ─┬─▶ mutate the model's snapshot   (the next frame already shows it)
               ├─▶ push the inverse on the undo stack
               └─▶ Effect::Apply(Mutation) ─▶ store.queue() ─▶ kick a push
                                                    │
                                                    └─▶ reload ─▶ Msg::TasksLoaded
```

The optimistic edit lands in `Model` before the effect runs, because a task list that
waits for SQLite to answer before showing a tick is the lag this project exists to
remove. The reload that follows is confirmation, not the mechanism.

A rejected write already has its path: `Sync` emits `SyncFailed`/`Rejected`, the store
rolls the change back, and the model reloads and toasts. That was built and tested in
Phase 2 and Phase 4 adds nothing to it.

## Undo

A `Vec<Mutation>` of inverses in the `Model`, and a redo stack beside it. Undo pops,
queues it as an ordinary mutation, and pushes *its* inverse onto redo. Any new edit clears
redo, as everywhere else.

Session-scoped, deliberately. A persistent stack sounds strictly better until an inverse
built yesterday meets a task the server has changed since — the rollback then fights a
newer truth, and the failure is confusing in a way that "the stack starts empty" is not.

## `criax add`

```sh
criax add 'Call the VA *urgent !3 +Legal tomorrow'
```

Parses with the same `criax-core::quickadd`, applies and queues through the same
`Store::queue`, then pushes if the server is reachable. If it is not, it says the task is
queued and the next run syncs it — which is the whole local-first thesis, in a form that
works from a script or a phone over ssh.

Safe alongside a running TUI: the store is WAL with a 5s busy timeout, so the two
processes do not fight. The TUI will not *show* the new task until its next sync or `r`,
which is worth knowing but not worth a watcher.

## Keys

```
d      toggle done          a   add a task
x      delete               e   edit (form)
p      priority             D   due date
l      labels               m   move to project
u      undo                 C-r redo
Space  configured quick action  (QUICK_ACTIONS.md)
```

Every one of these is a row in `KEYMAP`, so the help modal and the `:` palette pick them
up for free.

## Order of work

1. The write path, `d`, and undo/redo — the loop end to end, provable.
2. Quick-add: the prompt with live parse feedback, then `criax add`.
3. The quick keys and the `Space` actions.
4. The edit form.
5. Delete.
