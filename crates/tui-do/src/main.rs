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

/// Identities of the production instance, as digests rather than names.
///
/// Mirrors `tui-do-api`'s live-test guard deliberately: prod holds real task data, and a
/// client pointed at it by a stale config would happily write. The flag exists so the
/// answer is "yes, I meant it" rather than "there was no way to say no".
///
/// A hostname is not the only way to name a machine, and this guard used to check only the
/// name. A config pointing at production *by IP address* reached exactly the same server
/// with exactly the same data and no guard at all — found on 2026-08-31 and recorded as
/// BUG-6. Both of prod's addresses are covered because both route to it: the tailnet one
/// from anywhere on the tailnet, the LAN one from the house.
///
/// **These are digests so the repository does not publish the fleet's naming, and that is
/// obfuscation rather than secrecy.** A hostname is low-entropy and a digest of one falls
/// to a dictionary attack in seconds. What it buys is that the names are not greppable, not
/// indexed by code search and not readable at a glance, which is the whole of the
/// requirement. Anything that needed to be secret would not be in a public repository.
const PROD_HOST_DIGESTS: &[u64] = &[0xd46d_34ba_49c7_e934];

/// The same machine, by address.
const PROD_ADDR_DIGESTS: &[u64] = &[0xf44a_8a55_290f_bec7, 0xe71f_ea92_f3dc_2f65];

/// FNV-1a, 64-bit.
///
/// A non-cryptographic hash on purpose. Hashing a hostname is obfuscation whichever
/// function is used, so the choice is between a dependency that implies a security property
/// this cannot have, and six lines that imply nothing. It is `const` so the digests above
/// can be checked against a literal at compile time in a test.
const fn digest(value: &str) -> u64 {
    let bytes = value.as_bytes();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

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
    ///   tui-do add '+[Dinner Places] Book a table'
    ///   tui-do add '+"Dinner Places" Book a table'
    ///   tui-do add '*[needs review] Draft the memo'
    ///
    /// Brackets are usually the easier of the two from a shell, because the shell strips
    /// quotes before tui-do ever sees them.
    ///
    /// EXAMPLES
    ///
    ///   tui-do add 'Renew the domain *urgent !3 +Admin tomorrow'
    ///   tui-do add 'File the quarterly return +[Life Admin] 27aug26'
    ///   tui-do add 'Water the plants every 3 days'
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
    if acknowledged || !names_production(url) {
        return Ok(());
    }
    bail!(
        "{url} is the production server, holding real task data.\n\
         Pass --i-know-this-is-prod if you mean it, or point server.url at the dev instance.\n\
         Nothing automated may write here: tests, seeding and resets all refuse this host."
    )
}

/// Whether this URL reaches the production instance, by either of its names.
///
/// The check is against the URL's **host**, not the URL text. Substring-matching the whole
/// string was wrong in both directions: it missed `https://203.0.113.7:8443`, which is
/// prod, and it would have refused `https://dev.example.com/?note=prod-box`, which is not.
///
/// A name matches when it *is* a production host or when its first label is one, so both
/// `prod-box` and `prod-box.example.net` are caught while `prod-box-notreally.example.com`
/// is not. The port and the path never take part.
fn names_production(url: &str) -> bool {
    matches_any(url, PROD_HOST_DIGESTS, PROD_ADDR_DIGESTS)
}

/// Whether `url` names a host in `hosts` or an address in `addrs`, comparing digests.
///
/// Split out from [`names_production`] so the matching rules can be tested against
/// fabricated hosts. Testing them against the real ones would put back exactly the names
/// the digests exist to keep out of the source.
///
/// A name matches when it *is* one of `hosts` or when its **first label** is, so both
/// `prod-box` and `prod-box.example.net` are caught while `prod-box-notreally.example.com`
/// is not. The port and the path never take part.
fn matches_any(url: &str, hosts: &[u64], addrs: &[u64]) -> bool {
    let Some(host) = host_of(url) else {
        // Unparseable. Waving an unreadable URL through would be a worse answer than a
        // false positive, so every hostname-shaped run of characters in it is checked.
        //
        // This is narrower than the substring search it replaces -- a digest cannot be
        // searched for inside a longer string -- so a prod name welded into a longer word
        // no longer matches. No URL shape produces that; a URL that cannot be parsed at all
        // is already the unusual case, and the parsed path above is what runs in practice.
        return url
            .to_ascii_lowercase()
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
            .any(|token| label_matches(token, hosts));
    };
    if addrs.contains(&digest(&host)) {
        return true;
    }
    label_matches(&host, hosts)
}

/// Whether a hostname, or its first label, digests to one of `hosts`.
fn label_matches(host: &str, hosts: &[u64]) -> bool {
    if host.is_empty() {
        return false;
    }
    let first_label = host.split('.').next().unwrap_or(host);
    hosts.contains(&digest(host)) || hosts.contains(&digest(first_label))
}

/// The host of a URL, lowercased, without its port, path or trailing dot.
fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    // `example.com.` and `example.com` are the same name; a trailing dot is how you say
    // "fully qualified" and is not part of the identity.
    Some(host.trim_end_matches('.').to_string())
}

/// Say, on stderr, which credential files other accounts on this machine can read.
///
/// SEC-2. `Config::exposed_credential_files` has been able to answer this all along and the
/// interface has always asked -- `runtime::run` toasts it at startup. The add path never did,
/// so a host driven entirely through `tui-do add`, which is what the README recommends for
/// agent use, was never told. Found on 2026-09-06 installing the rc on `arm-host-1`: `scp` without
/// `-p` creates the destination under the *receiving* account's umask rather than carrying
/// `0o600` across, and `add` reached the server, printed `Sent.` and said nothing.
///
/// The same helper the interface uses, deliberately -- a second way of deciding what counts
/// as exposed is a second thing to keep true. Every entry is printed rather than only the
/// first, which is what a toast is limited to: an inline token in a permissive config and a
/// permissive token file are different fixes, and naming one hides the other.
///
/// Goes to stderr so `Sent.` keeps stdout to itself and anything reading that output is
/// unaffected. Failures to write are dropped: a warning that cannot be printed is not a
/// reason to refuse to add a task.
fn report_exposed_credentials(config: &Config, config_path: &Path, out: &mut impl std::io::Write) {
    for problem in config.exposed_credential_files(config_path) {
        let _ = writeln!(out, "warning: {problem}");
    }
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
    report_exposed_credentials(&config, &path, &mut std::io::stderr());

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

    /// BUG-6: the guard matched the URL *text*, so prod-by-IP walked straight through.
    ///
    /// Proved on 2026-08-31 by starting tui-do against prod's tailnet address with no
    /// `--i-know-this-is-prod` and no complaint. Every row here is a way of naming the same
    /// machine that holds real task data.
    #[test]
    fn every_way_of_naming_production_is_refused() {
        // Fabricated hosts, not the real ones. The guard's *rules* are what this test is
        // about, and asserting them against the real production names would put those
        // names back into the source that the digests exist to keep them out of.
        const HOSTS: &[u64] = &[digest("prod-box")];
        const ADDRS: &[u64] = &[digest("203.0.113.7"), digest("192.0.2.19")];

        for url in [
            "https://prod-box.example.net:8443",
            "https://PROD-BOX.example.net:8443",  // case
            "https://prod-box.example.net:8443/", // trailing slash
            "https://prod-box.example.net./",     // fully-qualified trailing dot
            "https://prod-box.example.net",       // no port
            "http://prod-box:8443",               // short name
            "https://203.0.113.7:8443",           // by address -- BUG-6
            "https://192.0.2.19:8443",            // the other address
            "not even a url prod-box",            // unparseable, still caught
        ] {
            assert!(
                matches_any(url, HOSTS, ADDRS),
                "{url} was not recognised as production"
            );
        }
    }

    /// The acknowledgement flag must actually work, or it would be a lie. Driven through
    /// the real guard, because that is the wiring under test -- with a URL that is not
    /// production, since a passing flag has to be a no-op there too.
    #[test]
    fn the_flag_is_honoured_and_harmless() {
        assert!(guard_production("https://vikunja.example.com", true).is_ok());
        assert!(guard_production("https://vikunja.example.com", false).is_ok());
    }

    /// The digest constants must not silently become empty. A guard that matches nothing
    /// refuses nothing, and every other test here uses fabricated values, so nothing else
    /// would notice.
    #[test]
    fn the_production_digests_are_populated_and_stable() {
        assert!(
            !PROD_HOST_DIGESTS.is_empty(),
            "no production host is guarded"
        );
        assert_eq!(
            PROD_ADDR_DIGESTS.len(),
            2,
            "both addresses reach production"
        );
        // Known answer: a change to `digest` would silently stop matching the constants,
        // which are literals computed with this exact function.
        assert_eq!(digest("tui-do"), 0x315a_0487_6d29_cecd);
        assert_eq!(digest(""), 0xcbf2_9ce4_8422_2325);
    }

    /// The other direction: the guard must not cry wolf, or it trains people to pass the
    /// flag by reflex — at which point it protects nothing.
    #[test]
    fn everything_that_is_not_production_still_starts() {
        const HOSTS: &[u64] = &[digest("prod-box")];
        const ADDRS: &[u64] = &[digest("203.0.113.7")];

        for url in [
            "https://dev-box.example.net:8443", // the dev instance
            "https://198.51.100.4:8443",        // dev by address
            "http://localhost:3456",
            "https://vikunja.example.com",
            // The old check substring-matched the whole URL, so this was refused for a
            // word in its query string.
            "https://dev.example.com/?note=prod-box",
            // A different machine whose name merely begins the same way.
            "https://prod-box-notreally.example.com",
        ] {
            assert!(
                !matches_any(url, HOSTS, ADDRS),
                "{url} was wrongly treated as production"
            );
        }
    }

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

    /// SEC-2: `tui-do add` never said a credential file was readable by other accounts.
    ///
    /// `Config::exposed_credential_files` has answered this all along, and the interface has
    /// always asked -- `runtime::run` toasts it at startup. The add path did not, so a host
    /// driven entirely through `tui-do add` -- which is what the README recommends for agent
    /// use -- was never told. Found on 2026-09-06 installing the rc on `arm-host-1`, where `scp`
    /// without `-p` had created the token under the receiving account's umask: `add` reached
    /// the server, printed `Sent.` and said nothing at all.
    #[cfg(unix)]
    #[test]
    fn the_add_path_says_when_a_credential_file_is_readable_by_others() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("tui-do-sec2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.yaml");
        let token_path = dir.join("token");

        std::fs::write(&token_path, "tk_secret\n").unwrap();
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let config = Config::from_yaml(&format!(
            "server:\n  url: https://vikunja.example\n  token_file: {}\n",
            token_path.display()
        ))
        .unwrap();

        let mut out = Vec::new();
        report_exposed_credentials(&config, &config_path, &mut out);
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains(&token_path.display().to_string()),
            "the exposed token file was not named: {printed:?}"
        );
        assert!(
            printed.contains("chmod 600"),
            "no remedy offered: {printed:?}"
        );

        // And a token only its owner can read is not worth a word.
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut quiet = Vec::new();
        report_exposed_credentials(&config, &config_path, &mut quiet);
        assert!(quiet.is_empty(), "noise on a private token: {quiet:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
