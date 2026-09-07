# tui-do — the macOS port: what it actually took, 2026-09-07

Built and run on `sw-mba`: Apple Silicon (arm64), macOS 26.6.2, Darwin 25.6.0.

`README.md` lists macOS under **Coming** as "a planned port … It may well build today, but
it is not tested or supported yet; that should be a quick gap to close." That estimate was
correct, and this file is the record of closing it — kept because the interesting part is
not the two lines of source that changed, but the three things that needed no source change
at all and still stopped tui-do working.

**The headline: it compiles on macOS with zero source changes, and the whole test suite
passes. Everything that was actually broken was outside the compiler's reach** — one missing
binary, one environment check that silently inverted, and a set of paths that differ by
platform and are documented only in their Linux form.

---

## 1. Compiling: nothing was needed

No source edits, no dependency changes, no feature flags, no vendored anything.

| | |
|---|---|
| `rustc` / `cargo` | 1.98.0, `aarch64-apple-darwin` (workspace needs ≥ 1.85) |
| C toolchain | Apple clang 21.0.0, Command Line Tools — for `libsqlite3-sys`, which builds SQLite from source |
| `cargo build --release` | exit 0, **zero warnings**, 1m15s cold |
| `cargo test --workspace` | **708 passed, 0 failed** across all 18 test binaries |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| Artifact | `Mach-O 64-bit executable arm64`, 11 MB |

That is with the workspace's own gate live — `unsafe_code = "forbid"`,
`unwrap_used = "deny"`, `panic = "deny"` — so the result is not "it built with warnings
suppressed".

Two things the README calls out as Linux requirements turn out to be non-issues here rather
than problems solved:

- **No OpenSSL.** TLS is rustls, and `rustls-platform-verifier` reads the macOS trust store
  directly. Nothing to install.
- **No system SQLite.** `rusqlite`'s `bundled` feature compiles it, which is why a C
  toolchain is required at build time and nothing is required at run time.

The `#[cfg(unix)]` blocks in `main.rs`, `store/mod.rs` and `config/mod.rs` all apply on
macOS, so the permission-mode handling they guard is live rather than compiled out. The
clipboard needed nothing: it is OSC 52 escape sequences (`runtime::osc52`), not `wl-copy`
or `xclip`, so it is the terminal emulator's problem on every platform equally. That design
choice — made for the Tailscale fleet — paid for the macOS port without anyone intending it.

---

## 2. Source changes: two, both in `crates/tui-do/src/runtime/mod.rs`

18 insertions, 4 deletions, one file. Both are the same underlying mistake: **assuming
freedesktop.**

### 2.1 `xdg-open` does not exist on macOS

`Effect::OpenUrl` (`runtime/mod.rs:297`) spawned `xdg-open` by name, and quoted that name in
both of its error strings. macOS has `open` for the same job.

Fixed with a per-platform constant rather than a `cfg` block around the call, so the command
and the two error messages that name it cannot drift apart:

```rust
#[cfg(target_os = "macos")]
const URL_OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
const URL_OPENER: &str = "xdg-open";
```

Confirmed in the shipped artifact: `strings target/release/tui-do` matches `open` and does
not contain `xdg-open`.

### 2.2 `url_action()` sent every Mac down the copy path

This is the one worth reading twice, because it would not have looked like a bug — it would
have looked like tui-do working as documented.

`url_action()` (`runtime/mod.rs:722`) decides whether `o` opens a link or copies it:

```rust
if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
    return UrlAction::Copy;
}
```

**macOS sets neither variable, and always has a window server.** So `o` would have copied,
every time, on a machine perfectly able to open the link — and it would have said so in a
toast, exactly the way the README describes the intended behaviour on a headless SSH box.
A user would reasonably have read that as correct.

The `SSH_CONNECTION` / `SSH_TTY` check above it is the one that still carries meaning on
macOS, and is untouched. Only the X11/Wayland check is compiled out:

```rust
#[cfg(not(target_os = "macos"))]
if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
```

### The diff

```diff
@@ -297,10 +297,10 @@ Effect::OpenUrl(url) => {
             // `spawn_blocking` rather than `tokio::process`, which would need another
-            // tokio feature for one call. `xdg-open` hands the URL to a handler and exits
+            // tokio feature for one call. The opener hands the URL to a handler and exits
             // immediately, so this waits on a fork-exec and not on a browser.
             tokio::task::spawn_blocking(move || {
-                let result = std::process::Command::new("xdg-open")
+                let result = std::process::Command::new(URL_OPENER)
@@ -310,8 +310,8 @@
                 let failure = match result {
                     Ok(status) if status.success() => None,
-                    Ok(status) => Some(format!("xdg-open exited with {status}")),
-                    Err(error) => Some(format!("could not run xdg-open: {error}")),
+                    Ok(status) => Some(format!("{URL_OPENER} exited with {status}")),
+                    Err(error) => Some(format!("could not run {URL_OPENER}: {error}")),
@@ -723,12 +723,26 @@ fn url_action() -> UrlAction {
     if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
         return UrlAction::Copy;
     }
+    // A local macOS session always has a window server to open onto, and sets neither of
+    // these -- checking for them here would send every Mac down the copy path and never
+    // open anything. The `SSH_*` check above is the one that still matters there.
+    #[cfg(not(target_os = "macos"))]
     if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
         return UrlAction::Copy;
     }
     UrlAction::Open
 }
+
+/// The program that hands a URL to the desktop.
+///
+/// `xdg-open` is the freedesktop entry point, and does not exist on macOS, which has `open`
+/// for the same job. Named once so the command and the error messages that quote it cannot
+/// drift apart.
+#[cfg(target_os = "macos")]
+const URL_OPENER: &str = "open";
+#[cfg(not(target_os = "macos"))]
+const URL_OPENER: &str = "xdg-open";
```

### How the Linux branch was checked

Only `aarch64-apple-darwin` is installed, and adding a musl target would need a cross C
toolchain for the bundled SQLite — disproportionate for an eighteen-line patch. Instead the
two `cfg` attributes were **inverted in place** and the workspace recompiled, so this Mac
built the configuration Linux takes (`xdg-open`, plus the `DISPLAY` check live).
`cargo clippy --workspace --all-targets -- -D warnings` exited 0 with no warnings, then the
file was restored. That type-checks the branch this platform would otherwise never compile.

---

## 3. Paths: the part with no source change and the most impact

**Nothing here is a bug. It is a documentation gap, and it is the thing that will waste
someone's afternoon.**

tui-do locates its directories through the `dirs` crate — `config_dir()` at
`config/mod.rs:215`, `data_dir()` at `store/mod.rs:101`. On Linux those follow XDG. On
macOS they return Apple-native locations, and `dirs` deliberately ignores `XDG_CONFIG_HOME`
there. So every path in the README's setup section is wrong on this platform:

| | README says (correct on Linux) | Actual on macOS |
|---|---|---|
| Config | `~/.config/tui-do/config.yaml` | `~/Library/Application Support/tui-do/config.yaml` |
| Token | `~/.config/tui-do/token` | `~/Library/Application Support/tui-do/token` |
| Store | `~/.local/share/tui-do/tui-do.db` | `~/Library/Application Support/tui-do/tui-do.db` |

The failure this produces is mild but genuinely misleading: you follow **REQUIRED — First
time setup** exactly, write the files where it says, and tui-do still stops with
`could not read …/config.yaml: No such file or directory` — naming a path the README never
mentions. The error is honest; the instructions that led to it were not.

`dirs::state_dir()` also returns `None` on macOS, but `main.rs:186` already falls back to
`data_local_dir()`, so logging needed nothing.

**This was left unfixed on purpose.** Whether macOS should follow XDG (one path across the
fleet, README correct everywhere, config portable by `scp`) or Apple convention (what
`dirs` gives, what Mac users expect) is a product decision, not a port detail. Both are
defensible; see §6.

Two escape hatches exist today and neither needs a rebuild:
`TUI_DO_CONFIG` (`config/mod.rs:38`) overrides the config path, `TUI_DO_DB`
(`store/mod.rs:96`) the database, and `--config` beats both.

---

## 4. Getting the config and token onto the box

`~/tui-do-keys.sh` on `sw-x1` is the fleet's procedure for handing another box this one's
config and the token that config names. It could not be used as-is, for three reasons — the
first structural, the other two both consequences of §3.

It **pushes**: it runs on the box that has the credentials and `scp`s outward. Every
platform difference is on the receiving side, so the adaptation is the pull half, run on the
Mac. It lives at `~/tui-do-keys.sh` here, deliberately the same name.

1. **Destination directory.** The original hardcodes `~/.config/tui-do` on the far side.
   On macOS every file would land somewhere tui-do never reads, and it would *still* report
   the config missing — §3's failure, arrived at from a different direction.

2. **`token_file` is copied to the same absolute path on the far side.** The original says
   so in its own comments: *"That silently assumes the destination has the same username and
   home — true across this fleet."* It is not true here. sw-x1's config names
   `/home/swasko/.config/tui-do/prod-token`; macOS has no `/home/swasko` at all.

   **The fix turned out to be already built into tui-do.** `config::resolve_relative`
   (`config/mod.rs:402`) expands `~`, leaves absolute paths alone, and resolves anything
   *relative* against the directory of the config file itself. So the adapted script
   rewrites the line as it installs:

   ```diff
   -  token_file: /home/swasko/.config/tui-do/prod-token
   +  token_file: prod-token
   ```

   The config and its token now travel to any box on either platform with no third path to
   keep in step — and nothing has to quote a config directory that, on this platform,
   contains a space. This is strictly better than what the original does on Linux too.

   Worth noting because the original's comment says the opposite: it claims `~` "is expanded
   by whatever writes the config, not by tui-do." `resolve_relative` and its test
   `a_tilde_token_file_expands_to_the_home_directory` show tui-do does expand it. The
   comment is stale, and following it is what produced the absolute-path approach.

3. **The landed config was `cat`ed to the terminal**, which prints the credential whenever
   the token is inline rather than in its own file — a case the original explicitly detects
   and warns about, then exposes anyway. Everything the adapted script displays passes
   through a mask first.

Two further changes, neither forced by the platform:

- **`ssh` drains stdin**, so piping a `y` to the confirmation prompt gave `read` an EOF and
  it silently took the `[y/N]` default. `-n` on all three `ssh` calls. The original is only
  ever driven by hand from a terminal, so it never had to care.
- **The write probe became a read.** The original's closing instructions suggest
  `tui-do add "rc probe … - delete me"` to prove the setup works — which writes a real task
  to whichever server the config names, to be deleted there. The adapted script instead does
  a read-only `GET /api/v1/user` and reports the authenticated username. The token reaches
  curl down a pipe rather than in `argv`, where `ps` would show it.

---

## 5. What was verified, and what was not

Verified:

- Build, test, clippy and fmt as in §1 — re-run after the patch, same results.
- The Linux `cfg` branch still lints clean (§2).
- `open` is in the binary, `xdg-open` is not.
- Config installed at the macOS path; `config.yaml` and the token both `0600`, directory
  `0700` — which is what `config::restrict_to_owner` would tighten them to anyway, and what
  `config::world_readable` (`config/mod.rs:389`) warns about if looser.
- The install rewrites **only** the `token_file` line: diffed against the source config,
  comments and the `sync:` and `view:` blocks byte-identical.
- The token authenticates against the configured server, read-only.
- tui-do loads the config end-to-end: the missing-config error is gone, it proceeds to
  create `tui-do.db`, and the only remaining failure is `could not put the terminal into raw
  mode` — expected when stdin is `/dev/null` rather than a tty.

**Confirmed by hand, 2026-09-07:** `o` opens a URL on this Mac. That closes §2.2, which was
the one thing nothing automated could reach — it is a change to *which branch is taken*, so
only a human pressing the key settles it. The `open` half of §2.1 is settled with it.

**Still not verified, and this remains the honest limit of this port:** the rest of the
interface has not been driven on macOS. Everything else above is the compiler, the test suite
and the startup path. Nothing here has exercised rendering, keybindings, the sync engine
against a live server from inside the interface, OSC 52 clipboard behaviour in Terminal.app
or iTerm2, behaviour across a lid close, or `o`'s *copy* branch over SSH — which, now that
the open branch works, is the one that keeps a browser from opening on an unattended Mac.

`md/2026-09-07-macos-test-plan.md` is the plan for closing the rest, and its exit criteria
are what should gate moving macOS out of "planned" in the README's platform table.

---

## 6. Open decisions, deliberately not taken

- **XDG or Apple paths on macOS (§3).** The one that matters. Making `config_dir()` and
  `store::path()` prefer XDG on macOS would put the fleet on one path, make the README
  correct everywhere, let `tui-do-keys.sh` skip its first adaptation, and make a config
  portable by plain `scp`. Keeping `dirs` is what a Mac user expects and needs no code. Both
  are reasonable; the port should not decide it quietly.
- **README setup section.** Whatever §3 resolves to, the setup instructions currently
  produce an error on macOS. If the paths stay Apple-native they need a macOS column.
- **`install -Dm755` in the README** is GNU coreutils. BSD `install` has no `-D`, so the
  documented install line fails on macOS; it needs
  `mkdir -p ~/.local/bin && install -m755 target/release/tui-do ~/.local/bin/tui-do`.
- **The `xdg-open` comment in `tui-do-keys.sh` on sw-x1** is stale about tilde expansion
  (§4.2) and worth correcting at the source, since it is what makes the absolute-path
  approach look necessary.
