//! criax — a fast, local-first terminal client for Vikunja.

// The binary is the one place user-facing stdout/stderr is correct: `--version`, `doctor`
// output, and fatal errors printed after the terminal has been restored.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use clap::Parser;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(name = "criax", version, about, long_about = None)]
struct Cli {
    /// Path to a config file, overriding the default lookup.
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    println!("criax {} (scaffold)", env!("CARGO_PKG_VERSION"));
    if let Some(path) = cli.config.as_deref() {
        println!("config: {}", path.display());
    }
    Ok(())
}
