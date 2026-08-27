//! Build tasks for tui-do.
//!
//! Run with `cargo xtask <task>` (see `.cargo/config.toml` for the alias).
//!
//! - `fetch-spec` download `/api/v1/docs.json` from a live server into `spec/vikunja.json`
//!
//! Models are hand-written rather than generated; `tui-do-api/tests/conformance.rs`
//! checks them against the spec instead. See `tui-do-api/src/models/mod.rs` for why.

// xtask is a developer tool: printing to the terminal is its entire output.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Where the spec is fetched from unless `TUI_DO_DEV_URL` overrides it.
///
/// Deliberately the *dev* instance. Production is read-only and must not become a
/// routine dependency of the build.
const DEFAULT_SPEC_SOURCE: &str = "https://dev-box.example.net:8443";

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("fetch-spec") => fetch_spec(),
        Some(other) => bail!("unknown task: {other}"),
        None => {
            println!("usage: cargo xtask fetch-spec");
            Ok(())
        }
    }
}

/// Repository root, derived from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// Download the OpenAPI document from a live Vikunja and write it to `spec/vikunja.json`.
///
/// The spec is ground truth for every endpoint the client calls, so it comes from a
/// running server rather than from a copy that can silently go stale. cria shipped a
/// bundled spec that fell 22 paths behind and still documented `/tasks/all`, an endpoint
/// upstream had renamed — which is the breakage this task exists to prevent.
fn fetch_spec() -> Result<()> {
    let base = std::env::var("TUI_DO_DEV_URL").unwrap_or_else(|_| DEFAULT_SPEC_SOURCE.to_string());
    let url = format!("{}/api/v1/docs.json", base.trim_end_matches('/'));
    let out = repo_root().join("spec/vikunja.json");

    println!("fetching {url}");
    let result = Command::new("curl")
        .args(["-sSf", "--max-time", "60", &url])
        .output()
        .context("failed to run curl; is it installed?")?;

    if !result.status.success() {
        bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }

    // Parse before writing, so a proxy error page can never land in the spec file.
    let doc: serde_json::Value =
        serde_json::from_slice(&result.stdout).context("response was not valid JSON")?;

    let paths = doc
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .map_or(0, serde_json::Map::len);
    let definitions = doc
        .get("definitions")
        .and_then(serde_json::Value::as_object)
        .map_or(0, serde_json::Map::len);
    if paths == 0 {
        bail!("document has no paths; that is not a Vikunja OpenAPI spec");
    }

    let version = doc
        .pointer("/info/version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");

    // Pretty-print with a stable key order so refreshes produce reviewable diffs
    // rather than one enormous reformatted line.
    let pretty = serde_json::to_string_pretty(&doc)?;
    std::fs::write(&out, pretty.as_bytes())
        .with_context(|| format!("failed to write {}", out.display()))?;

    println!(
        "wrote {} — Vikunja {version}, {paths} paths, {definitions} definitions",
        out.display()
    );
    println!("review the diff: an endpoint that moved is an upstream change worth reading");
    Ok(())
}
