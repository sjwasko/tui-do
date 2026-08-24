# criax — working notes for Claude

A local-first terminal client for Vikunja. Rust workspace, ratatui UI, SQLite store.
Full phase plan in `PLAN.md`.

## The rules that matter

**1. The render loop never awaits I/O.**
`criax-tui` is pure and synchronous. `update(&mut Model, Msg) -> Vec<Effect>` describes side
effects as values; the effect runtime in `crates/criax` executes them and sends results back
as `Msg`. `criax-tui` has no `reqwest`, no `rusqlite`, no `tokio` dependency, and must never
gain one — that dependency ban *is* the enforcement mechanism.

*Why:* the project criax replaces awaited network calls while holding a lock on its
application state, freezing the terminal for the duration of every slow request.

**2. No `show_x_modal: bool` fields.**
Screen and modal state is `enum Screen` plus a `Vec<Modal>` stack. Adding a modal means
adding one enum variant.

*Why:* cria's `App` struct has ~100 fields including 22 `show_*_modal` bools paired with
`Option<Modal>` values, so illegal states are representable and every modal needs a branch in
a 790-line function.

**3. No endpoint is called that isn't in `spec/vikunja.json`.**
That file is the OpenAPI document fetched from a live server (`/api/v1/docs.json`). A
conformance test asserts every path template the client builds exists in it. Refresh the spec
with `cargo xtask fetch-spec` when the server is upgraded.

*Why:* cria hardcoded `/tasks/all`, which upstream renamed to `/tasks`, and its response was
a 195-line "Method 1 / Method 2 / Method 3" fallback chain that guessed at endpoints.

**4. Pagination is never assumed.**
Read `x-pagination-total-pages` and `x-pagination-result-count`; take the page cap from
`/api/v1/info`'s `max_items_per_page` (50 on our server) rather than hardcoding it.

*Why:* cria requests `per_page=10000`, is silently capped at 50, and drops tasks past the
first page without telling anyone.

**5. Writes are optimistic.**
`update` mutates the local store immediately and queues an outbox entry. On rejection the
sync engine emits `Msg::SyncFailed`, which rolls back and toasts. Undo/redo rides on this
mechanism rather than a parallel one.

## Environment

| | |
|---|---|
| **Dev server** | `https://dev-box.example.net:8443` — use this for everything |
| **Prod server** | `https://prod-box.example.net:8443` — **read-only, always.** Never a write target, never a test target |
| **Vikunja version** | v2.5.0 (dev pinned to match prod) |
| **TLS** | Tailscale Serve certs are publicly trusted; never disable certificate verification |

Reset the dev server to its seeded baseline with `deploy/reset-dev.sh`. Seed it from a prod
export with `deploy/seed-from-prod.sh` (which reads prod and writes only to dev).

`crates/criax` refuses to start against the prod URL without `--i-know-this-is-prod`, and
integration tests refuse to run unless `CRIAX_TEST_URL` points at dev. Do not weaken either
guard to make something pass.

## Platform policy

Linux only through GA — tested on Omarchy (Arch, this workstation) and Ubuntu 26.04 LTS (via
`ubuntu:26.04` container). macOS is a post-GA port; there is a non-gating `cargo check` in CI
purely to limit drift. Windows is answered with "use WSL" and is not a build target.

Still write portably where it is free: paths via `dirs`, never cwd-relative writes,
`rusqlite` with `bundled`. Platform-varying behavior (URL opening, markdown rendering,
clipboard, `$EDITOR`) goes behind a small trait with a Linux impl — that trait is the seam
the macOS port uses later.

## Commands

```sh
cargo build --workspace
cargo clippy --workspace --all-targets    # must be clean; CI runs with -D warnings
cargo fmt --all
cargo test --workspace
cargo xtask fetch-spec                    # refresh spec/vikunja.json from the dev server
deploy/test-ubuntu.sh                     # build + test in the ubuntu:26.04 container
```

Workspace lints deny `unwrap`, `panic`, `todo`, `dbg!` and forbid `unsafe`. Tests may
`allow` them at module level; production code may not.

## Reference material

`../cria` is the predecessor, checked out for reference. Read it to learn *what* a screen
shows or *how* the quick-add syntax behaves — its `tests/` are a useful behavioral spec. Do
not copy its code: it carries no license (no `LICENSE` file was ever committed), and its
architecture is the thing criax exists to replace.

The Vikunja web UI is the design reference for layout and interaction.
