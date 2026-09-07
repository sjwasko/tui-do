# tui-do — where the API token lives: the design, before any code

Written 2026-09-07, after the macOS port shipped in `v1.0.1` and a Homebrew tap was
published. Prompted by [veans](https://vikunja.io/docs/veans/), an experimental
Vikunja CLI in Vikunja's own documentation, which stores its credential in the OS keychain
and authenticates by OAuth. Both ideas were examined; **one of them is available and one is
not**, and the difference was settled by asking the server rather than by reading either
document.

Nothing here is built yet. This is the note that gets argued with first.

---

## What prompted it, and what the measurement said

veans documents three credential sources, tried in order: **OS keychain**
(macOS Keychain, Windows Credential Manager, libsecret/gnome-keyring on Linux), then
`VEANS_TOKEN`, then `~/.config/veans/credentials.yml` at mode `0600`. It also documents
**OAuth 2.0 with PKCE against Vikunja's built-in authorization server** as its default,
"requiring no client registration".

That second one would be worth more than everything else in this note put together, so it
was checked first.

**It is not available on the Vikunja tui-do is built against.** Measured against dev on
2026-09-07, running `v2.5.0`:

| asked | answered |
|---|---|
| `/.well-known/oauth-authorization-server` | `200`, `text/html` |
| `/.well-known/openid-configuration` | `200`, `text/html` |
| `/oauth/authorize` | `200`, `text/html` |
| `/api/v1/oauth/authorize` | `405` |
| a deliberately nonsense path | `200`, `text/html` |

**The `200`s are the web application's catch-all**, not endpoints. The body returned for
`.well-known/oauth-authorization-server` is byte-identical to the body returned for
`/definitely-not-a-real-endpoint-9f3a` — same md5 — and both are the Vikunja SPA's
`index.html`. A status code alone would have said the opposite of the truth here, which is
the whole reason this project measures rather than probes.

`spec/vikunja.json` agrees and was right all along: of 126 paths the only auth surfaces are
`/login`, `/user/token`, `/user/token/refresh`, the scoped `/tokens` family, and
`/auth/openid/{provider}/callback` — which is Vikunja acting as an OIDC *client* to an
external provider, not as an authorization server. `/api/v1/info` reports
`openid_connect.enabled: false`.

**So OAuth is deferred to a server upgrade, not designed here.** Re-probe after the next
one; `veans` claims to need only Vikunja ≥ 2.4.0, which sits oddly with this and is worth
resolving before anyone builds against it. If it does arrive it deletes the whole "Token
permissions" section of the README, which is the largest single piece of onboarding friction
tui-do has.

**The keychain needs nothing from the server.** It is entirely client-side and can be built
today. The rest of this note is about that.

---

## Why this is worth doing at all

Not because a token file is insecure. Because **a file has a mode, and a mode is a thing
that goes wrong silently**.

This project has already paid for that twice, and both are recorded:

- **SEC-1** — the store was `0644` in a `0755` directory, so every task title was readable
  by any account on the machine. `config::restrict_to_owner` already existed and had never
  been applied to it.
- **SEC-2** — `scp` without `-p` creates the destination under the *receiving* account's
  umask rather than carrying `0600` across. Found installing `v1.0.0-rc.1` on
  `arm-host-1`: the token landed world-readable, `tui-do add` reached the server, printed
  `Sent.`, and said nothing. The `chmod 600` was done by hand, from memory.

**A keychain item has no mode to get wrong.** The entire class disappears — no umask, no
`scp -p`, no `~` that vanished in a paste, no `chmod` anybody has to remember. That is a
better argument than "secrets should be in a keychain", and it is the one to keep.

The second reason is narrower and specific to how tui-do now reaches people: **`brew install`
brings users who have read nothing.** Asking them to `printf` a token into a file and
`chmod 600` it is the least Mac-shaped instruction in the README, and it is the first thing
they meet.

---

## The decision: add a source, do not replace one

**Keychain first, then the environment variable, then the file.** Exactly veans's order,
arrived at independently for reasons this project can name.

```
1. OS keychain          -- if present and unlocked
2. TUI_DO_API_TOKEN     -- already exists; CI, containers, one-off overrides
3. server.token_file    -- already exists; the fleet's ordinary case
4. server.token         -- already exists, already warned about, unchanged
```

**The file must stay, and this is not a compatibility concession.** It is the case tui-do is
actually used in. The fleet is boxes reached over SSH — `sw-x280`, `sw-pi`, `sw-mini-pi`,
`sw-hp3` — and on a headless Linux box a Secret Service daemon is usually absent, and when
present is usually *locked*, because unlocking it conventionally happens at graphical login.
A design that treats the keychain as the primary store and the file as legacy would break
every machine this software was written for.

So the keychain is **strictly additive**: a source that is tried first and skipped when it
is not there. No existing config becomes invalid. Nothing is migrated automatically.

### What "skipped when it is not there" has to mean precisely

Three different things can happen when the keychain is consulted, and they are not the same:

| | meaning | what tui-do does |
|---|---|---|
| **no keychain service** | headless Linux, no libsecret | fall through silently |
| **service present, locked** | logged in but the ring is not open | fall through, and **say so once** |
| **service present, no entry** | first run, or the user keeps it in a file | fall through silently |
| **service present, entry, read fails** | a real error | **report it, do not fall through** |

The middle two are the trap. A locked keyring that falls through *silently* to a file that
does not exist produces "no config" when the real answer is "your keyring is locked" — a
diagnosis the user cannot reach from the message. And an entry that exists but cannot be
read must never be quietly replaced by a different credential, because that is how someone
ends up authenticated as the wrong account without noticing.

---

## What it costs, honestly

**A new dependency**, and it is not a small one. The `keyring` crate pulls
platform-specific backends: Security.framework on macOS, `zbus`/libsecret on Linux, the
Windows credential API. The workspace currently has 412 crates in its lockfile and
`cargo-deny` gates licences and advisories on every push, so this is a real addition to
review, not a line in a manifest.

**It is the first dependency that is platform-conditional.** Everything so far builds the
same everywhere. This one wants `[target.'cfg(...)'.dependencies]`, and that is a shape the
release pipeline has never built — including through `cross` for the aarch64 musl target,
where a libsecret backend has no business being linked into a static binary at all.

**That last point may decide the design.** A statically linked musl binary that dlopens
libsecret is a contradiction; the honest options are to compile the Linux keychain backend
out of the musl builds entirely, or to accept that the Linux keychain is only available in a
from-source build. Neither is decided here and **it should be measured before it is
chosen** — build one and look.

**A place to put it.** `Platform-varying behavior goes behind a small trait with a Linux
impl` is already CLAUDE.md's rule, and it names URL opening, markdown rendering, clipboard
and `$EDITOR`. Credential storage is the same shape and belongs behind the same seam. The
macOS port just demonstrated that seam works — `URL_OPENER` is two constants and a `cfg`.

---

## What must not happen

**No automatic migration.** Reading a token file and silently moving it into the keychain
means the user's credential moves without being asked, ends up somewhere they did not put
it, and is not where their config says it is. If migration is offered it is an explicit
command with output — `tui-do login`, or `tui-do token --store-in-keychain` — and it leaves
the file alone rather than deleting it.

**No silent precedence surprise.** If a token is in both the keychain and a file and they
differ, the user is authenticated as somebody. Which one wins must be documented in the
README's configuration reference, not merely implemented, and a mismatch is worth naming in
`tui-do doctor` if that is ever built.

**The token never reaches a log or a terminal.** `secret.rs` already exists for this and
`redacted()` in `main.rs` already covers the migrate path. Anything new routes through the
same helpers rather than growing a second way to be careful.

**No new prompt on the startup path.** Rule 1 is that the render loop never awaits I/O, and
an unlocked-keyring dialog is I/O with a human in it. The keychain is read once, before the
interface starts, on the same path the config is read on.

---

## The open questions, not decided here

1. **Does the Linux backend survive a static musl build?** Measure before choosing. This is
   the one that could make the whole feature macOS-only in practice.
2. **`tui-do login` or a flag on an existing command?** A subcommand is discoverable and is
   what the Homebrew caveats block would name. It is also the natural home for OAuth later,
   if the server ever grows it — which argues for the name now even if it only stores a
   pasted token today.
3. **Does the keychain entry hold the token, or the whole credential?** A token is enough
   today. Password-and-refresh-cookie auth exists in the client and nothing uses it.
4. **What does `brew install` tell people?** The caveats block currently prints the config
   path and the setup URL. If `tui-do login` exists, that becomes one line and the whole
   printf-and-chmod dance leaves the macOS story.
5. **Windows.** The `keyring` crate supports it; tui-do does not support Windows and answers
   it with WSL. Nothing changes, but the dependency will compile code for a platform this
   project does not test, which is worth knowing rather than discovering.

---

## What this is not

It is not a security fix. No credential tui-do handles today is exposed by a correctly
configured install, and both real findings — SEC-1 and SEC-2 — are fixed. This removes a
*class of configuration mistake* and makes the macOS install idiomatic. Claiming more than
that would be overselling it, and the README should not.
