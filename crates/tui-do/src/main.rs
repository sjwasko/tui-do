//! tui-do — a fast, local-first terminal client for Vikunja.

// The binary is the one place user-facing stdout/stderr is correct: `--version`, `doctor`
// output, and fatal errors printed after the terminal has been restored.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use clap::{Args, Parser, Subcommand};
use tui_do_core::config::{migrate, Config};

mod runtime;

/// What a redacted token is replaced with when a config is printed.
const REDACTED: &str = "<redacted — the real token is written to the file>";

/// Hosts that are the production instance.
///
/// Mirrors `tui-do-api`'s live-test guard deliberately: prod holds real task data, and a
/// client pointed at it by a stale config would happily write. The flag exists so the
/// answer is "yes, I meant it" rather than "there was no way to say no".
const PROD_HOSTS: &[&str] = &["prod-box"];

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(name = "tui-do", version, about, long_about = None)]
struct Cli {
    /// Path to a config file, overriding the default lookup.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Start against the production server anyway.
    #[arg(long, global = true)]
    i_know_this_is_prod: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands. Without one, tui-do starts the interface.
#[derive(Debug, Subcommand)]
enum Command {
    /// Add a task, in quick-add syntax, without opening the interface.
    ///
    /// Applies locally and queues for the server, exactly as the interface does. If the
    /// server cannot be reached the task is queued and the next run sends it, which is
    /// the point: `tui-do add` works on a plane.
    Add(AddArgs),

    /// Import a cria configuration into tui-do's own format.
    ///
    /// tui-do is a clean break rather than a drop-in replacement, so this is a one-way
    /// translation: it reads cria's config, writes tui-do's, and says what did not carry
    /// across.
    Migrate(MigrateArgs),
}

/// Options for `tui-do add`.
#[derive(Debug, Args)]
struct AddArgs {
    /// The task, in quick-add syntax: `Call the VA *urgent !3 +Legal tomorrow`.
    #[arg(required = true, num_args = 1.., value_name = "TEXT")]
    text: Vec<String>,

    /// Queue the task without trying to send it.
    #[arg(long)]
    offline: bool,
}

/// Options for `tui-do migrate`.
#[derive(Debug, Args)]
struct MigrateArgs {
    /// The cria config to read. Defaults to `~/.config/cria/config.yaml`.
    #[arg(long, value_name = "PATH")]
    from: Option<PathBuf>,

    /// Where to write the result. Defaults to tui-do's own config path.
    #[arg(long, value_name = "PATH")]
    to: Option<PathBuf>,

    /// Show what would be written without writing it.
    #[arg(long)]
    dry_run: bool,

    /// Overwrite an existing tui-do config.
    #[arg(long)]
    force: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Add(args)) => run_add(&args, cli.config.as_deref(), cli.i_know_this_is_prod),
        Some(Command::Migrate(args)) => run_migrate(&args, cli.config.as_deref()),
        None => start(cli),
    }
}

/// Load the config and hand over to the interface.
///
/// The runtime is only started here, so `main` itself stays synchronous and every early
/// failure -- an unreadable config, the production guard -- is reported to a terminal
/// that is still in its normal state.
fn start(cli: Cli) -> anyhow::Result<()> {
    let path = Config::resolve_path(cli.config.as_deref())?;
    let config = Config::load(&path).with_context(|| {
        format!(
            "could not read {}. Run `tui-do migrate` to import a cria config, \
             or write one following the example in the README.",
            path.display()
        )
    })?;

    guard_production(&config.server.url, cli.i_know_this_is_prod)?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?
        .block_on(runtime::run(config, path))
}

/// Refuse to start against production unless it was asked for explicitly.
fn guard_production(url: &str, acknowledged: bool) -> anyhow::Result<()> {
    let host = url.to_ascii_lowercase();
    if !PROD_HOSTS.iter().any(|prod| host.contains(prod)) || acknowledged {
        return Ok(());
    }
    bail!(
        "{url} is the production server, which is read-only by policy.\n\
         Point server.url at the dev instance, or pass --i-know-this-is-prod if you mean it."
    )
}

/// Add a task from the command line.
fn run_add(
    args: &AddArgs,
    config_override: Option<&Path>,
    acknowledged: bool,
) -> anyhow::Result<()> {
    let path = Config::resolve_path(config_override)?;
    let config =
        Config::load(&path).with_context(|| format!("could not read {}", path.display()))?;
    guard_production(&config.server.url, acknowledged)?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?
        .block_on(runtime::add(
            &config,
            &path,
            &args.text.join(" "),
            args.offline,
        ))
}

/// Translate a cria config and write it as tui-do's.
fn run_migrate(args: &MigrateArgs, config_override: Option<&Path>) -> anyhow::Result<()> {
    let from = match &args.from {
        Some(path) => path.clone(),
        None => migrate::cria_config_path()?,
    };
    if !from.exists() {
        bail!(
            "no cria config at {}. Pass --from if it lives somewhere else.",
            from.display()
        );
    }

    let to = match &args.to {
        Some(path) => path.clone(),
        // `--config` names the file tui-do would read, which is exactly where an import
        // should land, so the two options cannot disagree about the destination.
        None => Config::resolve_path(config_override)?,
    };

    let migration =
        migrate::from_cria_file(&from).with_context(|| format!("reading {}", from.display()))?;

    println!("read {}", from.display());
    for note in &migration.notes {
        println!("  note: {note}");
    }

    if args.dry_run {
        println!("\n--- {} (not written) ---", to.display());
        // Never print a real token: this goes to a terminal, its scrollback, and
        // whatever is capturing the session.
        println!("{}", redacted(&migration.config).to_yaml()?);
        return Ok(());
    }

    // Overwriting someone's configuration is not something to do because they typed a
    // command that sounded plausible.
    if to.exists() && !args.force {
        bail!(
            "{} already exists. Move it aside, or pass --force to overwrite it.",
            to.display()
        );
    }

    migration.config.save(&to)?;
    println!("wrote {}", to.display());

    if migration.config.server.token.is_some() {
        println!(
            "\nThe API token was copied into this file. Consider moving it to a file of \
             its own and pointing server.token_file at it — config files end up in \
             dotfile repositories."
        );
    }
    if !migration.notes.is_empty() {
        println!(
            "\n{} thing(s) did not carry across; see the notes above.",
            migration.notes.len()
        );
    }
    Ok(())
}

/// A copy of `config` with any inline token replaced.
///
/// Only for printing. The saved file gets the real one, because a config that needs the
/// user to paste their token back in has not migrated anything.
fn redacted(config: &Config) -> Config {
    let mut copy = config.clone();
    if copy.server.token.is_some() {
        copy.server.token = Some(REDACTED.to_string());
    }
    copy
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tui-do-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cria_config(dir: &Path) -> PathBuf {
        let path = dir.join("cria.yaml");
        std::fs::write(
            &path,
            "api_url: https://vikunja.example\napi_key: tk_secret_value\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn the_cli_parses_migrate_with_its_options() {
        let cli = Cli::try_parse_from([
            "tui-do",
            "migrate",
            "--from",
            "/tmp/cria.yaml",
            "--to",
            "/tmp/tui-do.yaml",
            "--dry-run",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Migrate(args)) => {
                assert_eq!(args.from, Some(PathBuf::from("/tmp/cria.yaml")));
                assert_eq!(args.to, Some(PathBuf::from("/tmp/tui-do.yaml")));
                assert!(args.dry_run);
                assert!(!args.force);
            }
            other => panic!("expected migrate, got {other:?}"),
        }
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let dir = scratch("dry-run");
        let from = cria_config(&dir);
        let to = dir.join("tui-do.yaml");

        run_migrate(
            &MigrateArgs {
                from: Some(from),
                to: Some(to.clone()),
                dry_run: true,
                force: false,
            },
            None,
        )
        .unwrap();

        assert!(!to.exists(), "a dry run created the file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_config_is_not_overwritten_without_being_asked() {
        let dir = scratch("no-clobber");
        let from = cria_config(&dir);
        let to = dir.join("tui-do.yaml");
        std::fs::write(&to, "server:\n  url: https://mine.example\n").unwrap();

        let args = MigrateArgs {
            from: Some(from),
            to: Some(to.clone()),
            dry_run: false,
            force: false,
        };
        let refused = run_migrate(&args, None).expect_err("it should refuse");
        assert!(refused.to_string().contains("--force"), "{refused}");
        assert!(std::fs::read_to_string(&to)
            .unwrap()
            .contains("mine.example"));

        let forced = MigrateArgs {
            force: true,
            ..args
        };
        run_migrate(&forced, None).unwrap();
        assert!(std::fs::read_to_string(&to)
            .unwrap()
            .contains("vikunja.example"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_cria_config_says_so_rather_than_writing_an_empty_one() {
        let dir = scratch("missing");
        let args = MigrateArgs {
            from: Some(dir.join("nothing-here.yaml")),
            to: Some(dir.join("tui-do.yaml")),
            dry_run: false,
            force: false,
        };
        let error = run_migrate(&args, None).expect_err("it should fail");
        assert!(error.to_string().contains("no cria config"), "{error}");
        assert!(!dir.join("tui-do.yaml").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_printed_config_does_not_carry_the_token_but_the_written_one_does() {
        // A dry run goes to a terminal, its scrollback, and whatever is recording the
        // session. The file is the only place the real token belongs.
        let dir = scratch("redaction");
        let from = cria_config(&dir);
        let to = dir.join("tui-do.yaml");
        let migration = migrate::from_cria_file(&from).unwrap();

        let printed = redacted(&migration.config).to_yaml().unwrap();
        assert!(!printed.contains("tk_secret_value"), "{printed}");

        run_migrate(
            &MigrateArgs {
                from: Some(from),
                to: Some(to.clone()),
                dry_run: false,
                force: false,
            },
            None,
        )
        .unwrap();
        assert!(std::fs::read_to_string(&to)
            .unwrap()
            .contains("tk_secret_value"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
