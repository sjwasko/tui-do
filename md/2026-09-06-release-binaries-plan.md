# Release Binaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship statically linked `tui-do` binaries for x86_64 and aarch64 Linux from a tag push, so nobody has to compile from source.

**Architecture:** One new GitHub Actions workflow triggered by a `v*` tag. It gates on the full test suite, builds one static musl binary per architecture, refuses to publish anything that is not actually static, and attaches tarballs plus `SHA256SUMS` to a GitHub Release. No production Rust changes — this is packaging and documentation.

**Tech Stack:** GitHub Actions, `dtolnay/rust-toolchain`, `musl-tools` (x86_64), `cross` or `cargo-zigbuild` (aarch64), `cargo-about`, `gh release`.

**Spec:** `md/2026-09-06-release-binaries-design.md`

## Global Constraints

- **Targets:** `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. Both ship at GA.
- **Binary name:** `tui-do` (`crates/tui-do/Cargo.toml`, `[[bin]]`).
- **Version lives once:** `Cargo.toml:6`, `[workspace.package] version`. Member crates use `version.workspace = true`. The three internal path dependencies at `Cargo.toml:23-25` each carry a literal `version = "0.1.0"` that **must be bumped in the same commit** — `cargo-deny`'s `wildcards = "deny"` is why they carry a version at all.
- **Artifact names:** `tui-do-<tag>-<target>.tar.gz`, e.g. `tui-do-v1.0.0-x86_64-unknown-linux-musl.tar.gz`.
- **Tarball contents:** `tui-do`, `README.md`, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-LICENSES.md`.
- **Action versions**, matching `.github/workflows/ci.yml`: `actions/checkout@v5`, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`.
- **Rust floor:** 1.85 (`rust-toolchain.toml`, `README.md`).
- **Never weaken the prod guard or the `TUI_DO_TEST_URL` guard** to make anything pass.
- **The repo is private until GA.** Every task before Task 7 must work on a private repo.

---

### Task 1: Prove the aarch64 musl build

The one unproven piece in the spec. Do this before writing a workflow that assumes it works — exactly as `x86_64` was probed on 2026-09-06.

**Files:**
- Create (throwaway): `.github/workflows/arm-probe.yml`

**Interfaces:**
- Consumes: nothing.
- Produces: a decision — `cross` or `cargo-zigbuild` — recorded in the spec, and consumed by **Task 3**.

- [ ] **Step 1: Create the probe branch**

```bash
git checkout -b probe/musl-arm
```

- [ ] **Step 2: Write the probe workflow**

Create `.github/workflows/arm-probe.yml`:

```yaml
# THROWAWAY. Probe only: can this tree cross-build for aarch64 musl?
# Delete with the branch.
name: arm probe

on:
  pull_request:
    branches: [main]

jobs:
  cross:
    name: cross build (aarch64-unknown-linux-musl)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: aarch64-unknown-linux-musl
      - name: install cross
        run: cargo install cross --locked
      - name: build
        run: cross build --release --bin tui-do --target aarch64-unknown-linux-musl
      - name: is it static, and is it arm?
        run: |
          f=target/aarch64-unknown-linux-musl/release/tui-do
          ls -l "$f"
          file "$f"
      - uses: actions/upload-artifact@v4
        with:
          name: tui-do-arm-probe
          path: target/aarch64-unknown-linux-musl/release/tui-do
```

- [ ] **Step 3: Push and open a PR to run it**

```bash
git add .github/workflows/arm-probe.yml
git commit -m "probe: can this tree cross-build for aarch64 musl"
git push origin probe/musl-arm
gh pr create -R sjwasko/tui-do --base main --head probe/musl-arm \
  --title "probe: aarch64 musl cross-build" \
  --body "Throwaway. Answers whether cross builds rusqlite's bundled SQLite for aarch64 musl. Not for merge."
```

The Forgejo push mirror carries the branch to GitHub within seconds (`sync_on_commit = 1`); wait for `gh api repos/sjwasko/tui-do/branches/probe/musl-arm` to succeed before creating the PR.

- [ ] **Step 4: Read the result**

```bash
gh run list -R sjwasko/tui-do --workflow "arm probe" --limit 1
```

Expected on success: `file` reports `ELF 64-bit LSB pie executable, ARM aarch64, ... static-pie linked`.

**If `cross` fails on the `rusqlite` build script**, replace Steps 2's build with `cargo-zigbuild` and re-run:

```yaml
      - name: install zig and cargo-zigbuild
        run: |
          pip install ziglang
          cargo install cargo-zigbuild --locked
      - name: build
        run: cargo zigbuild --release --bin tui-do --target aarch64-unknown-linux-musl
```

- [ ] **Step 5: Record the answer in the spec**

Edit `md/2026-09-06-release-binaries-design.md`. Under "Stage 2 — build", replace the `aarch64-unknown-linux-musl` bullet's "**Unproven…**" sentence with what actually happened: which tool worked, the `file` output, and the byte size. Under "Open questions", delete the `cross`/`rusqlite` question.

- [ ] **Step 6: Clean up and commit the spec update**

```bash
gh pr close <N> -R sjwasko/tui-do --comment "Answered; recorded in the spec. Throwaway branch deleted."
git checkout main
git branch -D probe/musl-arm
git push origin --delete probe/musl-arm
git add md/2026-09-06-release-binaries-design.md
git commit -m "Record which tool cross-builds tui-do for aarch64 musl"
git push origin main
```

---

### Task 2: Generate the third-party licence file

A static binary is a redistribution of every crate compiled into it, and every licence `deny.toml` admits requires attribution.

**Files:**
- Create: `about.toml`
- Create: `about.hbs`

**Interfaces:**
- Consumes: `deny.toml`'s allow-list (`deny.toml:7-20`).
- Produces: a working `cargo about generate about.hbs -o THIRD-PARTY-LICENSES.md`, consumed by **Task 3**.

- [ ] **Step 1: Install cargo-about locally**

```bash
cargo install cargo-about --locked
```

- [ ] **Step 2: Write `about.toml`**

The accepted list must match `deny.toml:7-20` exactly, or the release fails on a licence CI already allows:

```toml
# Which licences may appear in a shipped binary. Kept in step with `deny.toml`
# deliberately: that file decides what may enter the tree, and this one decides what
# is attributed when the tree is compiled into something we hand to a stranger. They
# must not drift -- a licence allowed there and missing here fails the release.
accepted = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "Zlib",
    "MPL-2.0",
    "CDLA-Permissive-2.0",
]

# Only what actually ships. The binary is Linux-only, so Windows and macOS
# dependencies in the lockfile are not redistributed and must not be attributed as
# though they were.
targets = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
]

# A crate with no license field at all is a stop, not a warning.
private = { ignore = true }
```

- [ ] **Step 3: Write `about.hbs`**

```handlebars
# Third-party licences

tui-do is distributed as a statically linked binary, which means the crates below are
compiled into it and redistributed with it. Their licences are reproduced in full, as
each of them requires.

The tui-do source itself is dual-licensed MIT OR Apache-2.0; see `LICENSE-MIT` and
`LICENSE-APACHE`.

{{#each licenses}}
## {{{name}}}

Used by:
{{#each used_by}}
- {{crate.name}} {{crate.version}}
{{/each}}

```
{{{text}}}
```

{{/each}}
```

- [ ] **Step 4: Run it and confirm it produces a real file**

```bash
cargo about generate about.hbs -o /tmp/THIRD-PARTY-LICENSES.md
wc -l /tmp/THIRD-PARTY-LICENSES.md
grep -c '^## ' /tmp/THIRD-PARTY-LICENSES.md
```

Expected: hundreds of lines and more than five `##` licence headings. **If it errors on an unlisted licence**, do not add that licence to `about.toml` alone — add it to `deny.toml` too, with a comment saying why it is acceptable, exactly as `Unicode-DFS-2016` was handled on 2026-09-05.

- [ ] **Step 5: Ignore the generated file**

The file is generated at release time and must not be committed. Append to `.gitignore`:

```
# Generated at release time by `cargo about`; never committed.
THIRD-PARTY-LICENSES.md
```

- [ ] **Step 6: Commit**

```bash
git add about.toml about.hbs .gitignore
git commit -m "Attribute the crates a static binary redistributes"
git push origin main
```

---

### Task 3: The release workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: `about.toml` and `about.hbs` from Task 2; the aarch64 build tool decided in Task 1.
- Produces: a GitHub Release carrying two tarballs and `SHA256SUMS`, consumed by Tasks 5 and 6.

- [ ] **Step 1: Write the workflow**

Replace `<ARM_BUILD_STEP>` with whichever Task 1 proved. For `cross`:
`cargo install cross --locked && cross build --release --bin tui-do --target ${{ matrix.target }}`

```yaml
name: Release

on:
  push:
    tags: ['v*']

# The release job creates a Release, which needs more than the default read token.
permissions:
  contents: write

jobs:
  # A tag can be pushed from any commit, including one CI has never seen. Without this
  # a release is an untested binary wearing a version number.
  gate:
    name: fmt + clippy + test
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace

  build:
    name: build ${{ matrix.target }}
    needs: gate
    runs-on: ubuntu-latest
    strategy:
      fail-fast: true
      matrix:
        include:
          - target: x86_64-unknown-linux-musl
            cross: false
          - target: aarch64-unknown-linux-musl
            cross: true
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ matrix.target }}

      - name: musl C toolchain, for the bundled SQLite
        if: ${{ !matrix.cross }}
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends musl-tools

      - name: build (native musl)
        if: ${{ !matrix.cross }}
        run: cargo build --release --bin tui-do --target ${{ matrix.target }}

      - name: build (cross)
        if: ${{ matrix.cross }}
        run: <ARM_BUILD_STEP>

      # A build that silently went dynamic must fail the release rather than ship: the
      # failure it causes lands on the user's machine, at startup, as a linker error.
      - name: refuse to ship anything that is not static
        run: |
          f=target/${{ matrix.target }}/release/tui-do
          file "$f"
          # The property that matters is "no dynamic interpreter", not which static
          # variant: a musl build may report `static-pie linked` or plain
          # `statically linked` depending on the target and the tool. Both pass;
          # anything dynamic fails, because that failure otherwise lands on the
          # user's machine at startup as a linker error.
          file "$f" | grep -q 'dynamically linked' \
            && { echo "DYNAMIC -- refusing to publish"; exit 1; }
          file "$f" | grep -Eq 'static-pie linked|statically linked' \
            || { echo "NOT STATIC -- refusing to publish"; exit 1; }

      - name: third-party licences
        run: |
          cargo install cargo-about --locked
          cargo about generate about.hbs -o THIRD-PARTY-LICENSES.md
          test -s THIRD-PARTY-LICENSES.md

      - name: stage the tarball
        run: |
          name="tui-do-${GITHUB_REF_NAME}-${{ matrix.target }}"
          mkdir -p "dist/$name"
          cp target/${{ matrix.target }}/release/tui-do "dist/$name/"
          cp README.md LICENSE-MIT LICENSE-APACHE THIRD-PARTY-LICENSES.md "dist/$name/"
          tar -C dist -czf "dist/$name.tar.gz" "$name"
          ls -l "dist/$name.tar.gz"

      - uses: actions/upload-artifact@v4
        with:
          name: dist-${{ matrix.target }}
          path: dist/*.tar.gz

  publish:
    name: publish
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: actions/download-artifact@v4
        with:
          pattern: dist-*
          merge-multiple: true
          path: dist
      - name: checksums
        run: |
          cd dist
          sha256sum *.tar.gz > SHA256SUMS
          cat SHA256SUMS
      # A tag containing a hyphen is a pre-release by semver convention -- v1.0.0-rc.1
      # publishes as one, v1.0.0 does not.
      - name: create the release
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          prerelease=""
          case "$GITHUB_REF_NAME" in *-*) prerelease="--prerelease" ;; esac
          gh release create "$GITHUB_REF_NAME" \
            --repo "$GITHUB_REPOSITORY" \
            --title "$GITHUB_REF_NAME" \
            --generate-notes \
            $prerelease \
            dist/*.tar.gz dist/SHA256SUMS
```

- [ ] **Step 2: Check the workflow parses**

```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml')); print('valid yaml')"
```

Expected: `valid yaml`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "Build and publish a static binary from a tag"
git push origin main
```

---

### Task 4: Rewrite the README's install path

**Files:**
- Modify: `README.md:31-59` (the `## Installing` section)

**Interfaces:**
- Consumes: the artifact names from Task 3.
- Produces: install instructions the rc is checked against in Task 6.

- [ ] **Step 1: Replace the section**

`README.md` currently opens `## Installing` with a paragraph that warns a C toolchain is required and says *"There are no published packages yet."* Both become wrong the moment Task 3 ships. Replace from `## Installing` down to (not including) `### Build prerequisites` with:

```markdown
## Installing

tui-do runs as a single binary with nothing to install beside it — SQLite is compiled in,
and the only external program it ever calls is `xdg-open`, when you press `o`.

### A prebuilt binary

Every release ships a **statically linked** binary. It needs no Rust, no C toolchain and no
system SQLite — only a Linux kernel — and the same file runs on Arch, Ubuntu, Debian, Fedora
and older LTS releases alike, because it depends on no system libc.

Pick the one matching `uname -m`: `x86_64` or `aarch64`.

```sh
tag=v1.0.0
arch=$(uname -m)          # x86_64 or aarch64
base=https://github.com/sjwasko/tui-do/releases/download/$tag

curl -fLO "$base/tui-do-$tag-$arch-unknown-linux-musl.tar.gz"
curl -fLO "$base/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS

tar -xzf "tui-do-$tag-$arch-unknown-linux-musl.tar.gz"
install -Dm755 "tui-do-$tag-$arch-unknown-linux-musl/tui-do" ~/.local/bin/tui-do
```

`sha256sum --check` is worth the extra line: it catches a truncated download, which
otherwise shows up as a confusing crash rather than as the incomplete file it is.

There is **no first-run wizard yet** — see [Configuring](#configuring) before the first
launch.

### Building from source

Only needed if you want to modify tui-do, or run on an architecture no release covers.
**SQLite is compiled from source**, so a C toolchain is required even though the resulting
binary needs none.
```

- [ ] **Step 2: Check the anchor is real**

```bash
grep -n '^## Configuring' README.md
```

Expected: one match. The install section links to `#configuring`; a broken anchor in the
first thing a new user reads is worse than no link.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Tell people to download a binary before telling them to compile one"
git push origin main
```

---

### Task 5: Cut v1.0.0-rc.1 and prove the pipeline

**Files:**
- Modify: `Cargo.toml:6` and `Cargo.toml:23-25`

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces: a published pre-release with two tarballs and `SHA256SUMS`, consumed by Task 6.

- [ ] **Step 1: Bump the version in both places**

`Cargo.toml:6` is `[workspace.package] version`. `Cargo.toml:23-25` are the three internal
path dependencies, which each carry a literal version so `cargo-deny`'s `wildcards = "deny"`
is satisfied. **All four move together or the build fails.**

```bash
sed -i '6s/^version = "0.1.0"$/version = "1.0.0"/' Cargo.toml
sed -i '23,25s/version = "0.1.0"/version = "1.0.0"/' Cargo.toml
grep -n '1\.0\.0' Cargo.toml
```

Expected: **four** lines — 6, 23, 24, 25.

Line-addressed rather than pattern-matched on the crate names, deliberately. The obvious
one-liner using alternation is `sed 's|\(tui-do-\(api\|core\|ui\)...|...|'`, and it
silently bumps **only line 6**: the `|` chosen as the `s` delimiter terminates the pattern at
the first `\|`. It reports success, `cargo build` still works because path dependencies
resolve by path, and the mistake surfaces later as a `cargo-deny` failure or a published
crate claiming the wrong version. Verified by running it on a copy on 2026-09-06.

Confirm nothing else moved:

```bash
diff <(sed 's/1\.0\.0/0.1.0/g' Cargo.toml) <(git show HEAD:Cargo.toml) \
  && echo "identical apart from the version bumps"
```

- [ ] **Step 2: Confirm the workspace still builds and the lockfile moved**

```bash
cargo build --workspace 2>&1 | tail -3
git diff --stat Cargo.lock
```

Expected: build finishes; `Cargo.lock` shows the version change.

- [ ] **Step 3: Commit and tag**

```bash
git add Cargo.toml Cargo.lock
git commit -m "Version 1.0.0"
git push origin main
git tag v1.0.0-rc.1
git push origin v1.0.0-rc.1
```

- [ ] **Step 4: Watch the release run**

```bash
gh run list -R sjwasko/tui-do --workflow Release --limit 1
```

Expected: `gate`, both `build` jobs, and `publish` all succeed.

- [ ] **Step 5: Confirm what was actually published**

```bash
gh release view v1.0.0-rc.1 -R sjwasko/tui-do --json isPrerelease,assets \
  --jq '.isPrerelease, (.assets[].name)'
```

Expected: `true`, then three assets — two `.tar.gz` and `SHA256SUMS`. **`isPrerelease` must
be `true`**; if it is false the hyphen detection in the publish job is wrong and `v1.0.0`
would be published from an untested path.

---

### Task 6: Drive the rc on real hardware

The whole reason musl was chosen. A binary that has only been built is a claim.

**Files:** none — this task runs on other machines.

**Interfaces:**
- Consumes: the pre-release from Task 5.
- Produces: the evidence GA bar item 2 needs, recorded in `md/MANUAL-CHECKS2.md`.

- [ ] **Step 1: Check the PocketTerm35's userland before trusting the arm artifact**

On the PocketTerm35:

```bash
uname -m
```

Expected: `aarch64`. **If it answers `armv7l`**, Raspberry Pi OS is running a 32-bit
userland and it cannot execute the arm64 binary at all. Stop, and record it — serving that
device needs a third target, `armv7-unknown-linux-musleabihf`, which is a scope decision for
the spec, not a flag.

- [ ] **Step 2: Install the rc on each host**

Run the README's own install commands from Task 4, with `tag=v1.0.0-rc.1`, on **`laptop`**
(x86_64), **`arm-host-1`** (aarch64), and the **PocketTerm35**. Using the README's commands rather
than a scratch script is deliberate: it checks the instructions, not just the artifact.

- [ ] **Step 3: Give each host a config before expecting it to reach anything**

There is no first-run wizard and no config is written for you, so a freshly installed host
stops with `could not read …/config.yaml: No such file or directory`. That is documented
behaviour, not a finding — but this task did not say to do it, and on 2026-09-06 `arm-host-1`
read as a failure twice because of it.

Copy a known-good config and token rather than typing them; see **Driving another box by
hand** in `CLAUDE.md` for why a pasted heredoc will not survive the trip.

```bash
scp workstation:~/.config/tui-do/config.yaml ~/.config/tui-do/config.yaml
scp workstation:~/.config/tui-do/token       ~/.config/tui-do/token
chmod 600 ~/.config/tui-do/token
cat ~/.config/tui-do/config.yaml
```

`workstation`'s config already points at **dev** and its `token_file` is an absolute path under
the same username and home, so it needs no editing on any of these hosts. `chmod` matters
because `scp` without `-p` creates the file under the destination's umask: tui-do notices a
world-readable token but reports it through `tracing::warn!` to a subscriber the CLI never
installs, so nothing would be printed either way.

Expected from `cat`: `url:` and `token_file:` on separate lines.

- [ ] **Step 4: Prove each one reaches the server**

On each host, against **dev**:

```bash
tui-do --version
tui-do add "rc probe from $(hostname) - delete me"
```

Expected: the version, then `Sent.` — which is DNS, TLS, auth and a write. `Queued.` instead
means it could not reach the server; that is a failure to investigate, not a pass.

- [ ] **Step 5: Confirm server-side, then clean up**

In the dev web UI, confirm one task per host, then delete them. A client reporting its own
success is not evidence.

- [ ] **Step 6: Record it**

Add a row to the "What has been driven, and when" table in `md/MANUAL-CHECKS2.md` naming each
host, its architecture, and the result. If the PocketTerm35 turned out to be 32-bit, record
that as the finding.

- [ ] **Step 7: Commit**

```bash
git add md/MANUAL-CHECKS2.md
git commit -m "Record the rc binaries driven on real hardware"
git push origin main
```

---

### Task 7: Cut v1.0.0

Only after Task 6 passes on every host.

**Files:**
- Modify: `CLAUDE.md` (the `## Commands` section)

**Interfaces:**
- Consumes: a green Task 6.
- Produces: the GA release.

- [ ] **Step 1: Tell the next reader how a release happens**

Append to `CLAUDE.md`'s `## Commands` section:

```markdown
**Releases are tag-triggered.** `git tag v1.2.3 && git push origin v1.2.3` runs
`.github/workflows/release.yml`, which gates on the full suite, builds a static musl binary
for `x86_64` and `aarch64`, refuses to publish one that is not `static-pie` linked, and
attaches both tarballs plus `SHA256SUMS` to a GitHub Release. A tag containing a hyphen
(`v1.0.0-rc.1`) publishes as a pre-release.

The version lives in `Cargo.toml` in **four** places — `[workspace.package] version` and the
three internal path dependencies, which carry a literal version because `cargo-deny`'s
`wildcards = "deny"` rejects a versionless path dependency. They move together.
```

- [ ] **Step 2: Commit, then tag the release**

```bash
git add CLAUDE.md
git commit -m "Say how a release is cut"
git push origin main
git tag v1.0.0
git push origin v1.0.0
```

- [ ] **Step 3: Confirm it is not a pre-release**

```bash
gh release view v1.0.0 -R sjwasko/tui-do --json isPrerelease,assets \
  --jq '.isPrerelease, (.assets[].name)'
```

Expected: `false`, and three assets.

- [ ] **Step 4: Make the repository public**

This is the step that makes the README's download URLs work for anyone. Do it **only** after
Task 6 passed and `v1.0.0` published — a public repo whose install instructions 404 is worse
than a private one.

```bash
gh repo edit sjwasko/tui-do --visibility public --accept-visibility-change-consequences
```

- [ ] **Step 5: Verify a stranger's view**

```bash
curl -fsSLI "https://github.com/sjwasko/tui-do/releases/download/v1.0.0/SHA256SUMS" \
  | head -1
```

Expected: `HTTP/2 200`. Run it **unauthenticated** — an authenticated success proves nothing
about the thing being fixed.

---

## Self-review

**Spec coverage.** Channel (Task 7 Step 4), musl linking (Tasks 1, 3), both architectures
(Tasks 1, 3), v1.0.0 on a tag (Tasks 5, 7), tarball contents including third-party licences
(Tasks 2, 3), the static-linkage gate (Task 3), the test gate (Task 3), README rewrite
(Task 4), `Cargo.toml` bump (Task 5), `CLAUDE.md` note (Task 7), rc-first (Task 5),
hand-driving on `laptop`/`arm-host-1`/PocketTerm35 including the 32-bit check (Task 6). No spec
section is unimplemented.

**Out of scope, and deliberately absent:** `.deb`, AUR, signing, `armv7` — except that Task 6
Step 1 detects whether `armv7` becomes necessary.

**Type consistency.** Artifact name `tui-do-<tag>-<target>.tar.gz` is used identically in
Task 3's staging step, Task 4's README, and Task 6's install. `THIRD-PARTY-LICENSES.md` is
spelled the same in Tasks 2 and 3. The four version sites in `Cargo.toml` are named
consistently in the Global Constraints, Task 5, and Task 7.
