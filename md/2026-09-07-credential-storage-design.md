# tui-do — where the API token lives: the design, before any code

Written 2026-09-07, after the macOS port shipped in `v1.0.1` and a Homebrew tap was
published. Prompted by [veans](https://vikunja.io/docs/veans/), an experimental
Vikunja CLI in Vikunja's own documentation, which stores its credential in the OS keychain
and authenticates by OAuth. Both ideas were examined against the server rather than taken
from either document — and the examination of one of them was botched, which is what the
banner below is about.

Nothing here is built yet. This is the note that gets argued with first.

> **Superseded in part, 2026-09-08.** The finding that OAuth is unavailable is wrong; the
> endpoints exist and work on `v2.5.0`. The correction, the two measurement mistakes that
> produced it, and what it does and does not change are in the section below. The keychain
> half of this note stands unaltered.

---

## What prompted it, and what the measurement said

veans documents three credential sources, tried in order: **OS keychain**
(macOS Keychain, Windows Credential Manager, libsecret/gnome-keyring on Linux), then
`VEANS_TOKEN`, then `~/.config/veans/credentials.yml` at mode `0600`. It also documents
**OAuth 2.0 with PKCE against Vikunja's built-in authorization server** as its default,
"requiring no client registration".

That second one would be worth more than everything else in this note put together, so it
was checked first.

~~**It is not available on the Vikunja tui-do is built against.**~~ **Wrong — see the
correction below.** Measured against dev on 2026-09-07, running `v2.5.0`, and kept here as
the record of how the wrong answer was reached:

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

~~`spec/vikunja.json` agrees and was right all along:~~ The spec is silent, which is not the
same as agreeing — of 126 paths the only auth surfaces are
`/login`, `/user/token`, `/user/token/refresh`, the scoped `/tokens` family, and
`/auth/openid/{provider}/callback` — which is Vikunja acting as an OIDC *client* to an
external provider, not as an authorization server. `/api/v1/info` reports
`openid_connect.enabled: false`.

**CORRECTION, measured 2026-09-08 — the conclusion above is wrong. OAuth is available
today, on this server, at `v2.5.0`.** The paragraph that stood here deferred the whole
question to a server upgrade. It should not have.

What settled it was asking the API rather than the web application:

| asked | answered |
|---|---|
| `GET /api/v1/oauth/authorize` | `405`, `{"message":"Method Not Allowed"}` |
| `OPTIONS /api/v1/oauth/authorize` | `204`, **`Allow: OPTIONS, POST`** |
| `OPTIONS /api/v1/oauth/token` | `204`, **`Allow: OPTIONS, POST`** |
| `GET /api/v1/oauth/garbage-xyz` | `404` — so the `405` is a *route*, not a prefix |
| `POST /api/v1/oauth/token` `{}` | `400`, code **`17007`**, "The grant_type is not supported. Use 'authorization_code' or 'refresh_token'." |
| `POST /api/v1/oauth/token` `grant_type=authorization_code` | `400`, code **`17004`**, "The authorization code is invalid or has already been used." |
| `POST /api/v1/oauth/authorize` `{}` | `401`, code `11`, invalid token — it authorizes *as* a signed-in user |

Distinct error codes for distinct failures, both `authorization_code` and `refresh_token`
grants named by the server itself, and form and JSON bodies both accepted. That is an
implemented authorization server, not a stub.

**Two mistakes produced the original answer, and both are worth keeping.**

**A single-page application serves the same HTML for every route it owns, real or
invented.** The original measurement compared the body of `/.well-known/oauth-authorization-server`
against a deliberately nonsense path, found the md5 identical, and concluded both were a
catch-all. The md5s *are* identical — re-measured, `d70965d24f69` for both — but that
proves only that the front end does client-side routing. It cannot distinguish a route the
SPA implements from one it does not, so it is not evidence of absence and was read as if it
were. `/oauth/authorize` returning the SPA is exactly what a browser-facing consent page
looks like.

**The probe never asked the API.** Every path in the original table was a front-end path
except one, and that one — `/api/v1/oauth/authorize` — answered `405`, which was filed
under "not available" alongside the `200`s. `405 Method Not Allowed` conventionally means
the route exists and the method was wrong, and here it does: the same prefix with a
nonsense suffix answers `404`, and `OPTIONS` names `POST`.

**`openid_connect.enabled: false` was cited as corroboration and is about something else.**
This note already says so two paragraphs above — that flag is Vikunja acting as an OIDC
*client* to an external provider. It is not a statement about Vikunja's own authorization
server, and counting it as agreement is how a wrong answer got a second vote.

**What this changes.** veans's documentation was right and this note was wrong: the
authorization server is there, which is why veans can require only ≥ 2.4.0. The "sits
oddly with this" hedge was the correct instinct and should have been resolved by
measurement before the conclusion was written down.

**It does not follow that tui-do should build OAuth next.** Two things stand in the way and
neither is about the server:

- **Rule 3.** No endpoint is called that isn't in `spec/vikunja.json`, asserted by a
  conformance test. The OpenAPI document — checked-in *and* re-fetched live on 2026-09-08,
  126 paths both — contains **no** `oauth` path at all. So the endpoints are real and
  undocumented, and using them means either an explicit, argued exception to Rule 3 or an
  upstream fix to the spec. That is a decision to take deliberately, not a detail to
  discover halfway through an implementation.
- **A browser.** The flow needs the user to open a URL, sign in, and paste a callback back.
  veans does exactly that. It is not hard, but it is a new interaction on the startup path
  and Rule 1's "no prompt on the startup path" applies to it.

The prize is unchanged and is still the largest one available: it deletes the whole "Token
permissions" section of the README, which is the biggest single piece of onboarding
friction tui-do has.

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
1. **Does OAuth displace this work, or sit beside it?** Now that the authorization server
   is known to exist (see the correction above), the ordering is a real question: a
   `tui-do login` that completes an OAuth flow and stores the result in the keychain is one
   feature, not two, and building the keychain first without deciding that is how the
   subcommand ends up with the wrong shape. Rule 3 has to be answered either way.
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
