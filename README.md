# tui-do

A fast, local-first terminal client for [Vikunja](https://vikunja.io).

tui-do keeps your tasks in a local SQLite store and reconciles with the server in the
background. It starts instantly, works with the server unreachable, and never blocks a
frame on a network call.

> **Status: pre-alpha.** The scaffold is in place; see `PLAN.md` for the phase breakdown.
> Not yet usable.

## Design

**The render loop never awaits I/O.** The UI is an Elm-style `Model` / `Msg` / `update` /
`view` loop that is pure and synchronous. Side effects are described as values and executed
by a separate async runtime that sends results back as messages.

This is enforced by the crate graph rather than by discipline:

| Crate | Responsibility | Notably cannot |
|---|---|---|
| `tui-do-api` | Typed go-vikunja REST client | — |
| `tui-do-core` | Domain model, SQLite store, sync engine, quick-add parser, config | render |
| `tui-do-ui` | `Model`, `Msg`, `update`, `view`, widgets, keymap | perform I/O — no `reqwest`, no `rusqlite`, no `tokio` |
| `tui-do` | CLI, wiring, effect runtime, terminal lifecycle | — |

Writes are optimistic: the local store updates immediately and the change is queued in an
outbox. If the server rejects it, the change is rolled back and reported.

Every endpoint the client calls is checked against `spec/vikunja.json` — the OpenAPI
document served by a live Vikunja at `/api/v1/docs.json` — by a conformance test, so an
upstream API change surfaces as a failing test rather than a runtime 404.

## Platform support

**Linux is the only supported platform through GA**, tested on Omarchy (Arch) and
Ubuntu 26.04 LTS.

- **macOS** — planned as a post-GA port. It may build today; it is not tested or supported.
- **Windows** — not a target. Use [WSL](https://learn.microsoft.com/windows/wsl/install) and
  run the Linux build. There are no plans to ship a native Windows binary.

## Building

```sh
cargo build --release
```

Task descriptions (Markdown, plain text, or HTML) render in process — no external tools
required.

## Relationship to cria

tui-do began as a fork-in-spirit of [cria](https://github.com/frigidplatypus/cria) by
frigidplatypus, which established the idea of a keyboard-driven Vikunja TUI along with its
quick-add syntax and column-layout configuration. tui-do is an independent implementation
with a different architecture and does not share cria's code.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.

## Copyright

Copyright is held collectively by the project's contributors ("The tui-do Authors");
attribution lives in the git history. Contributions are accepted under the dual
MIT/Apache-2.0 terms above.
