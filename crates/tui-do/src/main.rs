//! tui-do — a fast, local-first terminal client for Vikunja.

// The binary is the one place user-facing stdout/stderr is correct: `--version`, `doctor`
// output, and fatal errors printed after the terminal has been restored.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use clap::{Args, CommandFactory, Parser, Subcommand};
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
    ///
    /// SYNTAX
    ///
    /// Tokens may appear anywhere in the line and are taken out of the title. These are
    /// not the interface's keys — `p3` and `D` are ordinary words here, and stay in the
    /// title where you typed them.
    ///
    ///   +project          file it in a project, by title or id: +Legal, +#12
    ///   *label            attach a label that already exists: *urgent
    ///   @user             assign someone: @admin
    ///   !1 .. !5          priority, 1 lowest to 5 highest
    ///   a date            tomorrow, next friday, 27/08/26, 27aug26, 2026-08-27
    ///   due <date>        the same, said explicitly
    ///   start <date>      when work can begin
    ///   every <n> <unit>  repeat: every 2 weeks, every month
    ///
    /// NAMES WITH SPACES
    ///
    /// Wrap the value in brackets or quotes, which is what keeps the second word from
    /// falling back into the title:
    ///
    ///   tui-do add "+[Dinner Places] Book a table"
    ///   tui-do add '+"Dinner Places" Book a table'
    ///   tui-do add "*[needs review] Draft the memo"
    ///
    /// Brackets are usually the easier of the two from a shell, because the shell strips
    /// quotes before tui-do ever sees them.
    ///
    /// EXAMPLES
    ///
    ///   tui-do add "Call the VA *urgent !3 +Legal tomorrow"
    ///   tui-do add "Renew the passport +[Life Admin] 27aug26"
    ///   tui-do add "Water the plants every 3 days"
    // Verbatim, because the syntax above is a table and clap reflows a doc comment into
    // one paragraph by default -- which turns the whole of it into an unreadable run-on.
    #[command(verbatim_doc_comment)]
    Add(AddArgs),

    /// Import a cria configuration into tui-do's own format.
    ///
    /// tui-do is a clean break rather than a drop-in replacement, so this is a one-way
    /// translation: it reads cria's config, writes tui-do's, and says what did not carry
    /// across.
    Migrate(MigrateArgs),

    /// Print a shell completion script.
    ///
    /// Writes to stdout, so it is piped or redirected wherever the shell keeps them.
    /// This is what makes `tui-do add --<TAB>` offer `--help` and `--offline` rather
    /// than falling back to filenames.
    ///
    /// It is also the only way to get the greyed-out suggestion that appears ahead of
    /// the cursor. No program can paint that itself — it belongs to the shell, and each
    /// one wants something different:
    ///
    ///   fish    completions alone are enough; the suggestion is built in
    ///   zsh     needs the zsh-autosuggestions plugin, told to ask completions and not
    ///           only history:
    ///             ZSH_AUTOSUGGEST_STRATEGY=(history completion)
    ///   bash    completes on TAB; there is no inline suggestion to enable
    ///
    /// INSTALLING
    ///
    ///   fish  tui-do completions fish > ~/.config/fish/completions/tui-do.fish
    ///   zsh   tui-do completions zsh  > ~/.zfunc/_tui-do
    ///         # with ~/.zfunc on $fpath, ahead of compinit
    ///   bash  tui-do completions bash > ~/.local/share/bash-completion/completions/tui-do
    ///
    /// Re-run it after upgrading tui-do: the script is generated from the same command
    /// table as `--help`, so a stale one offers flags that have moved.
    #[command(verbatim_doc_comment)]
    Completions(CompletionsArgs),
}

/// Options for `tui-do add`.
#[derive(Debug, Args)]
struct AddArgs {
    /// The task, in quick-add syntax: `Call the VA *urgent !3 +Legal tomorrow`.
    ///
    /// See `tui-do add --help` for every token, and for how to name a project or label
    /// that has a space in it.
    // `Other` rather than the default, which lets a shell fall back to filenames: the
    // argument is a sentence, and offering the contents of the working directory to
    // someone typing a task title is worse than offering nothing at all.
    #[arg(
        required = true,
        num_args = 1..,
        value_name = "TEXT",
        value_hint = clap::ValueHint::Other
    )]
    text: Vec<String>,

    /// Queue the task without trying to send it.
    #[arg(long)]
    offline: bool,

    /// Create any label the task names that does not exist yet.
    ///
    /// Off by default: labels are one global pool shared by every project, so a typo in
    /// a task line otherwise becomes a permanent entry that pollutes completion
    /// everywhere. The interface asks; a command that may be running unattended cannot,
    /// so it takes an instruction instead.
    #[arg(long)]
    create_labels: bool,
}

/// Options for `tui-do completions`.
#[derive(Debug, Args)]
struct CompletionsArgs {
    /// Which shell to generate for.
    #[arg(value_name = "SHELL")]
    shell: clap_complete::Shell,
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

/// Send `tracing` output to a file, if the user asked for any.
///
/// Off unless `TUI_DO_LOG` is set, and never to stdout or stderr: the interface owns the
/// terminal, and a log line written into the alternate screen corrupts the frame the user
/// is looking at. `TUI_DO_LOG` takes the usual `RUST_LOG` syntax (`tui_do=debug`), and
/// `TUI_DO_LOG_FILE` overrides where it lands.
///
/// Until this existed there was no subscriber at all, so every `tracing::warn!` in the
/// workspace was discarded — including the one that tells a user their API token is
/// readable by other accounts, and the one guarding `Effect`'s `non_exhaustive`.
fn init_logging() -> Option<PathBuf> {
    let filter = std::env::var("TUI_DO_LOG").ok()?;
    let path = std::env::var("TUI_DO_LOG_FILE").map_or_else(
        |_| {
            dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .map(|dir| dir.join("tui-do").join("tui-do.log"))
        },
        |raw| Some(PathBuf::from(raw)),
    )?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::sync::Arc::new(file))
        .with_ansi(false)
        .try_init()
        .ok()?;
    tracing::info!(path = %path.display(), "logging started");
    Some(path)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging();

    match cli.command {
        Some(Command::Add(args)) => run_add(&args, cli.config.as_deref(), cli.i_know_this_is_prod),
        Some(Command::Migrate(args)) => run_migrate(&args, cli.config.as_deref()),
        Some(Command::Completions(args)) => {
            run_completions(args.shell);
            Ok(())
        }
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

/// Write a completion script for `shell` to stdout.
///
/// Generated from the same command table `--help` is built from, rather than written by
/// hand, so a flag cannot exist in one and not the other.
fn run_completions(shell: clap_complete::Shell) {
    let mut command = Cli::command();
    let name = command.get_name().to_string();
    clap_complete::generate(shell, &mut command, name, &mut std::io::stdout());
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
            args.create_labels,
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

    /// Generate a completion script into a string.
    fn completions(shell: clap_complete::Shell) -> String {
        let mut command = Cli::command();
        let mut out: Vec<u8> = Vec::new();
        clap_complete::generate(shell, &mut command, "tui-do", &mut out);
        String::from_utf8(out).expect("a completion script is text")
    }

    #[test]
    fn the_command_table_is_well_formed() {
        // clap's own audit -- duplicate flags, a long help that will not render, an
        // argument that cannot be reached. It costs nothing and it is the check that
        // fails first when a subcommand is added carelessly.
        Cli::command().debug_assert();
    }

    #[test]
    fn every_shell_gets_a_script_that_knows_the_subcommands() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
        ] {
            let script = completions(shell);
            for expected in ["add", "migrate", "completions", "offline"] {
                assert!(
                    script.contains(expected),
                    "the {shell} script does not mention {expected}"
                );
            }
        }
    }

    #[test]
    fn the_task_text_does_not_complete_to_filenames() {
        // The argument is a sentence. A shell offering the working directory to someone
        // typing a task title is worse than offering nothing, and it is what happens by
        // default -- an unhinted positional falls through to path completion.
        let script = completions(clap_complete::Shell::Bash);
        let add = script
            .split("tui__subcmd__do__subcmd__add)")
            .nth(1)
            .expect("the add subcommand has an arm");
        let arm = add.split("tui__subcmd").next().unwrap_or(add);
        // `--config` legitimately takes a path; nothing else in the arm may.
        let paths = arm.matches("compgen -f").count();
        assert_eq!(paths, 1, "only --config completes a path:\n{arm}");
    }

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

        // And only its owner may read it. `std::fs::write` creates at 0644, so the
        // migration that copies a token out of a cria config used to hand it to every
        // account on the machine -- while printing advice about dotfile repositories.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&to).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "the config carrying the token is readable by others: {mode:o}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
