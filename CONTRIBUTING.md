# Contributing to tui-do

Thank you for looking. Feedback on this project is genuinely wanted — more than patches are,
and the rest of this file explains why.

## Read this before opening a pull request

**GitHub is a read-only mirror.** Development happens on a private Forgejo instance, and
GitHub receives commits through a one-directional push mirror. Nothing merged on GitHub
reaches the canonical repository.

That means a pull request opened here **cannot be merged**. If it is a good change I would
have to re-apply it by hand and you would lose the commit attribution, and if it is not, you
will have spent an evening finding that out. Neither is a good trade, so please open an issue
first and I will tell you how to get the code to me.

This is a limitation of my setup, not a comment on your patch.

## What is most useful

In rough order of how much it helps:

### 1. Tell me what you were trying to do

Especially if you drive tui-do from a script or an agent. `add` is currently the whole
command-line surface; reading, completing and moving tasks are planned, and **which verbs get
built first is being decided by what people ask for.** A description of the thing you were
automating is worth more than a request for a specific flag.

### 2. Bug reports

[Open an issue](https://github.com/sjwasko/tui-do/issues) with:

- your distribution and version, and your architecture (`uname -m`)
- your Vikunja server version
- what you did, what happened, and what you expected instead
- anything tui-do printed, pasted verbatim rather than summarised

If it involves the interface rather than the CLI, your terminal and its size (`tput cols`,
`tput lines`) are often the whole answer — several bugs have turned out to be a window a few
rows shorter than a modal needed.

### 3. Opinions on undecided design

Some things are genuinely not settled, and are much cheaper to get right before they ship
than after people depend on them. Kanban is the big one: what an agent should *say* to move a
task across a board is an open question. If you work in Vikunja's board views, I would like
your opinion before it is built.

### 4. Code

Open an issue first. Small fixes are easy to take. Anything touching the architecture is
worth a conversation, because the constraints are unusual and mostly undocumented outside
`CLAUDE.md` — the render loop never awaits I/O, the UI crate is pure and may not gain an I/O
dependency, and no endpoint is called that is not in the vendored OpenAPI spec. A patch that
violates one of those is not wrong so much as aimed at a different program.

## What to expect from me

This is a one-person project that I use daily. I read everything. I will not necessarily
build everything, and I would rather tell you "no, and here is why" than leave an issue open
for a year.

## Security

Please do **not** open a public issue for a security problem. Email the address on my
[GitHub profile](https://github.com/sjwasko) instead, and give me a chance to fix it before
it is public.

## License

tui-do is dual-licensed under Apache-2.0 and MIT. Contributions are accepted under the same
terms.
