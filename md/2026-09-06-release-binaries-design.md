# Shipping binaries — design, 2026-09-06

**The goal, in the requester's words:** *"I do not want people (that don't want to) having to
compile from source."* So a release ships the source **and** a binary that runs without a
toolchain.

Nothing in this repository produces an artifact today. There are no tags, no release
workflow, and `README.md` says *"There are no published packages yet."* This is the design
for the thing that replaces that sentence.

## The problem this solves, measured rather than assumed

**A native build is not portable across the two target distros.** Arch on `workstation` carries
glibc **2.44**; Ubuntu 24.04 on `laptop` and `arm-host-1` carries **2.39**. A binary built here
does not load there — it fails at the dynamic linker with a missing `GLIBC_2.4x`, before
`main` runs.

So "ship a binary" needs a decision about linking, not just a build step.

## Decisions

| | decision |
|---|---|
| **Channel** | GitHub Releases on `sjwasko/tui-do`, which **goes public at GA** |
| **Linking** | One statically linked **musl** binary per architecture — no libc dependency at all |
| **Architectures** | **x86_64 and aarch64**, both at GA |
| **Version** | **v1.0.0**, and the workflow triggers on a `v*` tag |
| **Proof before GA** | A `v1.0.0-rc.1` pre-release, hand-driven on `laptop` and `arm-host-1` |

**Why musl rather than two glibc builds.** One artifact per architecture runs on Arch,
Ubuntu, Debian, Fedora and every older LTS, instead of two artifacts that each work on one
distro family and oblige the user to know which. It makes "no compiling" true generally
rather than for two named distros.

**This was verified, not assumed — 2026-09-06.** A throwaway CI job built
`x86_64-unknown-linux-musl` and the result is `ELF 64-bit LSB pie executable, static-pie
linked, stripped`, 14,230,304 bytes — within 0.5% of the glibc build. `ldd` answers
`statically linked`. The binary then ran **on the tailnet from `workstation`**: it resolved
`dev-box.example.net` through MagicDNS, completed the Tailscale Serve TLS
handshake, authenticated, and wrote a task to dev, which was confirmed server-side and
deleted. That last part is what CI could not have told us — musl's resolver is the usual
place a static Linux binary surprises you, and it did not.

The probe ran with an isolated `XDG_DATA_HOME`, and **where the store landed was checked**
rather than trusted: this project has a recorded case of a "fresh environment" test silently
using the real store because that variable was already set.

## What ships

Per architecture, one tarball; per release, one checksum file:

```
tui-do-v1.0.0-x86_64-unknown-linux-musl.tar.gz
tui-do-v1.0.0-aarch64-unknown-linux-musl.tar.gz
SHA256SUMS
```

Each tarball contains:

- `tui-do` — the static binary
- `README.md`
- `LICENSE-MIT`, `LICENSE-APACHE`
- `THIRD-PARTY-LICENSES.md`

**The licence files are an obligation, not a courtesy.** MIT requires its notice to travel
with "all copies or substantial portions"; Apache-2.0 §4 requires that recipients receive a
copy of the licence. A bare binary download would distribute the project without either.

**And a static binary is a redistribution of its dependencies.** Roughly 400 crates are
compiled *into* it, and `deny.toml` admits MIT, Apache-2.0, BSD-2/3-Clause, ISC, Unicode-3.0,
Unicode-DFS-2016, Zlib, MPL-2.0 and CDLA-Permissive-2.0 — **every one of which requires
attribution**. Generating `THIRD-PARTY-LICENSES.md` from the lockfile at release time
(`cargo-about`) is what turns "we only allow permissive licences" into actually honouring
them. It costs one CI step and it is the only part of this design nobody asked for.

## The pipeline

One workflow, `.github/workflows/release.yml`, triggered by `push: tags: ['v*']`.

**Stage 1 — gate.** `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
warnings`, `cargo test --workspace`. **A tag can be pushed from any commit, including one CI
has never seen**; without this a release is an untested binary wearing a version number.

**Stage 2 — build, once per architecture.**

- `x86_64-unknown-linux-musl` — `apt-get install musl-tools`, then plain `cargo build
  --release --target …`. Proven today.
- `aarch64-unknown-linux-musl` — needs a cross toolchain, because the bundled SQLite is C.
  **Proven 2026-09-06** with a throwaway probe workflow (PR
  [sjwasko/tui-do#2](https://github.com/sjwasko/tui-do/pull/2), CI run
  [34030154453](https://github.com/sjwasko/tui-do/actions/runs/34030154453)): `cross build
  --release --bin tui-do --target aarch64-unknown-linux-musl` succeeded on the first attempt,
  no `cargo-zigbuild` fallback needed. `file` reported `ELF 64-bit LSB executable, ARM
  aarch64, version 1 (SYSV), statically linked, stripped` — no dynamic interpreter, and
  `cross`'s musl toolchain produced a plain statically linked binary rather than a
  static-pie one (both are acceptable; only "dynamically linked" would have failed the
  probe). 11,317,992 bytes, comparable to the x86_64 build's ~14.2 MB. `cross` is the tool
  Stage 2 uses for this target.

**Stage 3 — prove, then publish.** For each artifact, assert it runs (`--version`) and that
it is *actually* static — `file` must say `static-pie` and `ldd` must not report a dynamic
interpreter. **A build that silently went dynamic must fail the release rather than ship**,
because the failure it causes lands on the user's machine, at startup, with a linker error.
Then `gh release create` with both tarballs and `SHA256SUMS`.

**A note for when the repo is public:** GitHub offers free arm64 runners (`ubuntu-24.04-arm`)
to public repositories, which would replace the aarch64 cross-compilation with a native build
*and* let CI smoke-test the arm binary. That is not available while the repo is private, and
the arm artifact must be testable before the flip — so cross-compilation is the GA answer and
the native runner is a simplification to take afterwards.

## What changes in the repository

**`README.md`** — the "Installing" section is rewritten. An **Install** subsection goes
*first*: download, verify against `SHA256SUMS`, `chmod +x`, move onto `PATH`. Build-from-source
moves below it.

It also needs a correction. The current text warns that a C toolchain is required — true when
**building**, and misleading now that installing is the common path. The release binary needs
nothing but a Linux kernel.

**`Cargo.toml`** — version `0.1.0` → `1.0.0` in the release commit.

**`CLAUDE.md`** — a short note that releases are tag-triggered and what the artifacts are.

## How a release is proved

Two things the pipeline cannot check for itself.

**1. Cut `v1.0.0-rc.1` first**, published as a GitHub **pre-release**. It exercises the entire
path — gate, both builds, checksums, upload, a real download URL — while claiming nothing.

**2. Drive the downloaded artifacts by hand**, which is the whole reason musl was chosen:

| host | arch | distro | what it proves |
|---|---|---|---|
| `laptop` | x86_64 | Ubuntu 24.04, glibc 2.39 | the x86 binary runs on a glibc older than the build host's |
| `arm-host-1` | aarch64 | Ubuntu 24.04, glibc 2.39 | the arm binary runs at all |
| PocketTerm35 | Raspberry Pi 5 | *to confirm* | the arm binary runs on hardware nobody built it on, at a terminal size nothing has been driven at |

**Check `uname -m` on the PocketTerm35 before assuming the arm artifact fits it.** A Pi 5 is
arm64 hardware, but Raspberry Pi OS still ships a 32-bit userland, and a 32-bit userland
answers `armv7l` and **cannot run an `aarch64` binary**. If it does, serving that device
needs a third target — `armv7-unknown-linux-musleabihf` — which is a scope decision, not a
build-flag change. Worth knowing before the release is cut rather than after.

On each: download the tarball, verify its checksum, run it against **dev**, and confirm a
write reaches the server. Downloading and running is the check — a binary that has only been
built is a claim.

**The PocketTerm35 is worth more than a third architecture check.** It is the smallest screen
this program will ever be asked to draw on, which makes it the right place to drive
`md/MANUAL-CHECKS2.md` section **E** (short windows) and to look hard at **BUG-11** — the
edit form has no scroll windowing, so on a short terminal `Tab` moves focus to a field that
has been clipped away and the caret is silently dropped. BUG-11 is open and was filed as
"verify first". A handheld terminal is where it stops being theoretical.

That is not a release blocker and does not belong in this pipeline. It is recorded here
because the device will be in someone's hands during the rc, and that is the cheapest the
observation will ever be.

This also gives **GA bar item 2** better evidence than the `ubuntu:26.04` container job does:
a real download, on a real machine, against a real server.

## Out of scope

- **`.deb` and AUR packaging.** Post-GA and feature-request-driven, matching what
  `skills/tui-do/SKILL.md` already promises about the agent CLI. An AUR `-bin` package would
  point at these release artifacts, so this design is a prerequisite rather than a competitor.
- **Signing** (gpg, sigstore). `SHA256SUMS` protects against a corrupted download and not
  against a compromised release. Worth a decision later; noting it so nobody mistakes a
  checksum for a signature.
- **32-bit, musl-libc variants beyond these two triples, and macOS.** macOS remains the
  post-GA port it already was.

## Open questions

- **Does `cargo-about` need a config file to satisfy every licence in the tree**, given
  `deny.toml` already enumerates the allowed set? Likely a small `about.toml`.
- **Which release does `jetson-box` care about?** It is arm64 like `arm-host-1`, so the same
  artifact should serve it. Untested, and not a GA blocker.
