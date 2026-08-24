//! Build tasks for criax.
//!
//! Run with `cargo xtask <task>` (see `.cargo/config.toml` for the alias).
//!
//! Tasks:
//! - `fetch-spec`     download `/api/v1/docs.json` from a live server into `spec/vikunja.json`
//! - `generate-models` turn the spec's definitions into serde structs in `criax-api`

#![allow(clippy::print_stdout, clippy::print_stderr)]

fn main() -> anyhow::Result<()> {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("fetch-spec") => {
            println!("not yet implemented: fetch-spec (Phase 1)");
            Ok(())
        }
        Some("generate-models") => {
            println!("not yet implemented: generate-models (Phase 1)");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown task: {other}"),
        None => {
            println!("usage: cargo xtask <fetch-spec|generate-models>");
            Ok(())
        }
    }
}
