//! tui-do — a fast, local-first terminal client for Vikunja.

// The binary is the one place user-facing stdout/stderr is correct: `--version`, `doctor`
// output, and fatal errors printed after the terminal has been restored.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use clap::{Args, CommandFactory, Parser, Subcommand};
use tui_do_core::config::{migrate, Config};

#[cfg(feature = "dynamic-completion")]
mod complete;
mod login;
mod runtime;

/// What a redacted token is replaced with when a config is printed.
const REDACTED: &str = "<redacted — the real token is written to the file>";

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(name = "tui-do", version, about, long_about = None)]
struct Cli {
    /// Path to a config file, overriding the default lookup.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

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
    /// Tokens may appear anywhere in the line. Labels, projects, users, priorities and
    /// repeats leave the title wherever they are. A date is always read, but leaves the
    /// title only when nothing follows it except whitespace, punctuation and other
    /// tokens; anywhere else it stays exactly as typed. So `call mom tomorrow` is titled
    /// "call mom", while `tomorrow buy milk` keeps its title and is still due tomorrow.
    ///
    /// These are not the interface's keys
    /// — `p3` and `D` are ordinary words here, and stay in the title where you typed
    /// them.
    ///
    ///   +project          file it in a project, by title or id: +Legal, +#12
    ///   *label            attach a label that already exists: *urgent
    ///   @user             assign someone: @admin
    ///   !1 .. !5          priority, 1 lowest to 5 highest
    ///   a date            tomorrow, next friday, 27aug26, aug 27, 2026-08-27
    ///   due <date>        the same, said explicitly; also 27.8, 27/08, 27/08/26
    ///   start <date>      when work can begin
    ///   every <n> <unit>  repeat: every 2 weeks, every month
    ///
    /// A date in digits alone needs `due` or `start` in front of it, unless it has a
    /// four-digit year: `call mom due 27.8` is due on 27 August, while `llvm 1.6 GB`
    /// and `add 1/2 cup` keep their numbers and get no date. A month name or a
    /// four-digit year works anywhere, except dotted with the year first (2026.1.5),
    /// which reads as a version: write 2026-01-05, or put `due` in front. Any date
    /// that was set is named in the confirmation line, so a misread one shows at once.
    ///
    /// CHECKING A LINE
    ///
    /// --dry-run prints what the line would create and the best way to write it, and
    /// queues nothing, writes nothing and contacts no server:
    ///
    ///   tui-do add --dry-run 'call the dentist tomorrow !3 +Personal'
    ///
    /// It checks projects and labels against the local store only. A project created
    /// elsewhere since the last sync can fail a dry run and still succeed for real,
    /// because a real add asks the server once before giving up.
    ///
    /// The best form is the title, then +project, *labels, @users and !N, then each
    /// date after its keyword in ISO form (due 2026-11-12, start 2026-11-09 at 09:00),
    /// then the repeat. Dates go last because a date leaves the title only when nothing
    /// follows it except whitespace, punctuation and other tokens. A real add prints
    /// the same line as "as typed best: ..." whenever what was typed differs from it;
    /// copy that form next time. A line starting with - is shown after "-- ", which
    /// keeps it from being read as a flag and goes outside any quotes.
    ///
    /// --tokens lists every project, label and user the local store knows, one to a
    /// line and written the way the best form writes them, then the token syntax:
    ///
    ///   tui-do add --tokens
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
    /// DESCRIPTION
    ///
    /// The line above is only the title and its tokens. A description is given one of
    /// three ways, and only one at a time:
    ///
    ///   -d, --description <TEXT>     inline, for a line or two; newlines are kept
    ///   --description-file <PATH>    read from a file; `-` reads stdin
    ///   -e, --edit                   write it in $VISUAL, else $EDITOR, else vi
    ///
    /// For anything long, move it rather than retype it: a file, a pipe, or a quoted
    /// heredoc, whose quotes ('EOF') keep the shell from expanding $ and backticks:
    ///
    ///   tui-do add 'Write the plan +Work' --description-file - <<'EOF'
    ///   ## Context
    ///
    ///   What was tried, and what is left.
    ///   EOF
    ///
    /// It is stored exactly as given -- Markdown, plain text or HTML -- but for one
    /// trailing newline and CRLF becoming LF from a file or stdin (-d keeps every
    /// byte), and quick-add tokens in it stay text. It must be UTF-8 and
    /// at most 64 KiB. The confirmation line counts it rather than printing it:
    /// `with a description (3 paragraphs, 1,240 characters)`.
    ///
    /// With --edit, the file ends in a `# ---- >8 ----` scissors line and help, as
    /// `git commit -v` does. Everything from that line down is ignored. Above it,
    /// blank lines at the start and just above the line are dropped, and the rest is
    /// kept exactly, `# Heading` lines included. Delete the scissors line and the whole
    /// file is kept, with a warning. The size limit counts only what is kept.
    ///
    /// EXAMPLES
    ///
    ///   tui-do add 'Renew the domain *urgent !3 +Admin tomorrow'
    ///   tui-do add 'File the quarterly return +[Life Admin] 27aug26'
    ///   tui-do add 'Water the plants every 3 days'
    ///
    /// WITH NO TEXT
    ///
    /// Run `tui-do add` alone at a terminal and it asks for the task on a few lines,
    /// showing what the line will create as you type:
    ///
    ///   Tab        completes +project, *label, @user, and a date after due or start;
    ///              a second Tab lists the matches
    ///   ?          straight after + * @ !, after `due ` or `start `, or on an empty
    ///              line, lists what can come next; anywhere else it is a question mark
    ///   F1         every token
    ///   Enter      adds it, exactly as `tui-do add '<the line>'` would
    ///   Esc        cancels, adding nothing
    ///
    /// The flags apply to the line it asks for: `tui-do add --dry-run` alone asks, then
    /// checks what was typed and adds nothing.
    ///
    /// The prompt is for people. From a script, pass the text.
    // Verbatim, because the syntax above is a table and clap reflows a doc comment into
    // one paragraph by default -- which turns the whole of it into an unreadable run-on.
    #[command(verbatim_doc_comment)]
    Add(AddArgs),

    /// Set tui-do up: write the config, store the API token, and check it works.
    ///
    /// This is the first-run path, and it replaces the `printf`, the `chmod 600` and the
    /// `$EDITOR config.yaml` in the README's setup section. Run it again whenever a token
    /// is rotated or a server moves — an existing config is read and updated, never
    /// replaced, so tuned columns and quick actions survive.
    ///
    /// It asks for the server, points you at Vikunja's API-token page, takes the token
    /// without echoing it, and then **asks the server who you are before writing
    /// anything**. That last step is the point: `other -> user` is the one permission
    /// tui-do cannot start without, and a token missing it otherwise fails later as a red
    /// toast that reads like the token is wrong rather than incomplete.
    ///
    /// The token is written to `token` beside the config, readable only by you, on every
    /// platform; only the directory differs.
    #[command(verbatim_doc_comment)]
    Login(LoginArgs),

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
    ///
    /// NAMES FROM THE STORE
    ///
    /// The script above completes flags and subcommands only. To have
    /// `tui-do add Call the VA +Le<TAB>` offer `+Legal`, use the dynamic script instead,
    /// which asks tui-do on every Tab. Add one line to the shell's startup file:
    ///
    ///   bash  ~/.bashrc                    source <(TUI_DO_COMPLETE=bash tui-do)
    ///   zsh   ~/.zshrc, after compinit     source <(TUI_DO_COMPLETE=zsh tui-do)
    ///   fish  ~/.config/fish/config.fish   TUI_DO_COMPLETE=fish tui-do | source
    ///
    /// It completes +project, *label and @user from the local store, a date word after
    /// `due` or `start`, and the flags as well. It reads the store and never writes,
    /// syncs or contacts a server; with no store it offers nothing. Generate it at shell
    /// start, as above, rather than saving it: the script and the binary speak a
    /// protocol that may change between versions.
    ///
    /// It completes unquoted words. Inside a quoted '...' argument, what happens differs
    /// by shell. `?` is not attempted in a shell; the `tui-do add` prompt has it.
    ///
    /// It needs tui-do's `dynamic-completion` feature, which is on by default. A build
    /// without it treats TUI_DO_COMPLETE as an ordinary variable and starts normally;
    /// use the static script above there.
    #[command(verbatim_doc_comment)]
    Completions(CompletionsArgs),
}

/// Options for `tui-do login`.
#[derive(Debug, Args)]
struct LoginArgs {
    /// The Vikunja server, skipping the prompt: `--url vikunja.example.com`.
    ///
    /// A bare host is accepted and `https` assumed, since the alternative would put a
    /// credential on a cleartext connection because somebody omitted five characters.
    #[arg(long, value_name = "URL")]
    url: Option<String>,

    /// Do not try to open the token page in a browser.
    ///
    /// Only ever attempted on a local desktop session anyway — over SSH `xdg-open` opens a
    /// browser on the wrong machine, so it is not tried. This is for saying no on a
    /// machine where it *would* be tried.
    #[arg(long)]
    no_browser: bool,
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
    //
    // Not `required`: with no text and a terminal at both ends, `tui-do add` asks for the
    // task instead. Without a terminal, `missing_text` gives clap's own error back.
    #[arg(
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
    /// so it takes an instruction instead. Without it, an unknown `*label` is left off and
    /// its words stay in the title as typed.
    #[arg(long)]
    create_labels: bool,

    /// Say what would be added, and the best way to write it, without adding it.
    ///
    /// Parses the line and resolves its project and labels against the local store
    /// exactly as a real add does, so a line that would fail fails the same way. Then
    /// prints what it would create and the line rewritten in its best form, and queues
    /// nothing, writes nothing and contacts no server. Projects are checked against the
    /// local store only, so one created since the last sync is not known to it.
    #[arg(long)]
    dry_run: bool,

    /// List the projects, labels and users a line may name, then the token syntax.
    ///
    /// One name to a line, grouped, each written exactly as the best form writes it:
    /// `+[Life Admin]` for a project with a space in it. From the local store only, so
    /// run tui-do once to sync first; adds nothing, writes nothing and contacts no
    /// server. For agents: run it once to learn the names, then --dry-run to check a line.
    // Plain text only. `--json` waits on item 3a's contract (md/TODO.md).
    #[arg(
        long,
        conflicts_with_all = [
            "text", "offline", "create_labels", "dry_run",
            "description", "description_file", "edit",
        ]
    )]
    tokens: bool,

    /// The task's description, inline: `-d 'Ask about the March invoice.'`
    ///
    /// Stored exactly as given, newlines included. Quick-add tokens in it are text, not
    /// tokens. For anything longer than a line or two, use --description-file.
    // Hyphen values allowed, because a Markdown list starts with `- `.
    #[arg(
        short = 'd',
        long,
        value_name = "TEXT",
        allow_hyphen_values = true,
        value_hint = clap::ValueHint::Other
    )]
    description: Option<String>,

    /// Read the task's description from a file, or from stdin if PATH is `-`.
    ///
    /// Stored exactly as given, but for one trailing newline, and CRLF line endings
    /// become LF. It must be UTF-8 text of 64 KiB or less. With `-`, pipe it in or use a quoted heredoc, and give the task's
    /// text as an argument.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "description",
        value_hint = clap::ValueHint::FilePath
    )]
    description_file: Option<PathBuf>,

    /// Write the task's description in your editor, as `git commit` does.
    ///
    /// Opens $VISUAL, else $EDITOR, else vi, on a temporary file only you can read,
    /// removed afterwards. Save an empty file to add no description; exit the editor
    /// with an error (:cq in vim) to add nothing at all. Needs a terminal.
    // Not with --dry-run: a description written only to be thrown away is lost work.
    #[arg(
        short = 'e',
        long,
        conflicts_with_all = ["description", "description_file", "dry_run"]
    )]
    edit: bool,
}

/// Read the description `args` names, if it names one, and check it.
///
/// Before the config or the store, so a file that is missing, too large or not text is
/// reported whatever else is wrong, and nothing is queued.
fn read_description(args: &AddArgs) -> anyhow::Result<runtime::DescriptionSource> {
    use tui_do_core::description;
    let bytes = match (&args.description, &args.description_file) {
        // Exactly as typed: the trailing newline `normalise` drops is a file's or a
        // heredoc's, and an argument has none it did not mean.
        (Some(text), _) => {
            return Ok(runtime::DescriptionSource::Given(description::check(
                text.clone().into_bytes(),
            )?));
        }
        (None, Some(path)) if path.as_os_str() == STDIN => {
            let stdin = std::io::stdin();
            // `-` at a terminal waits for typing, as `git commit -F -` does. Say so, or it
            // looks like a hang.
            if stdin.is_terminal() {
                eprintln!("Reading the description from stdin; end it with Ctrl-D.");
            }
            read_capped(stdin.lock()).context("could not read the description from stdin")?
        }
        (None, Some(path)) => {
            let file = std::fs::File::open(path).with_context(|| {
                format!("could not open the description file {}", path.display())
            })?;
            read_capped(file).with_context(|| {
                format!("could not read the description file {}", path.display())
            })?
        }
        (None, None) if args.edit => return Ok(runtime::DescriptionSource::Edit),
        (None, None) => return Ok(runtime::DescriptionSource::default()),
    };
    Ok(runtime::DescriptionSource::Given(description::normalise(
        bytes,
    )?))
}

/// What `--description-file` takes to mean stdin.
const STDIN: &str = "-";

/// Refuse `--description-file -` without the task's text.
///
/// Stdin is then the description, so it cannot also be the terminal a prompt for the
/// task would read from. Said here, in words, rather than left to a prompt that would
/// find its input already spent.
fn stdin_is_free(args: &AddArgs) -> anyhow::Result<()> {
    let reads_stdin = args
        .description_file
        .as_deref()
        .is_some_and(|path| path.as_os_str() == STDIN);
    if reads_stdin && args.text.is_empty() {
        bail!(
            "--description-file - reads the description from stdin, so give the task's \
             text as an argument: tui-do add 'Write the plan' --description-file -"
        );
    }
    Ok(())
}

/// Everything `reader` has, up to one byte past the size cap.
///
/// One past, so `normalise` can tell "exactly at the cap" from "over it" without a whole
/// misnamed log file being read into memory first.
fn read_capped(reader: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    let cap = u64::try_from(tui_do_core::description::MAX_BYTES).unwrap_or(u64::MAX);
    reader.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    Ok(bytes)
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
    // First: a shell asking for completions gets them and nothing else on stdout.
    #[cfg(feature = "dynamic-completion")]
    complete::handle(Cli::command);
    let cli = Cli::parse();
    init_logging();

    match cli.command {
        Some(Command::Add(args)) => run_add(&args, cli.config.as_deref()),
        Some(Command::Login(args)) => {
            login::run(args.url.as_deref(), args.no_browser, cli.config.as_deref())
        }
        Some(Command::Migrate(args)) => run_migrate(&args, cli.config.as_deref()),
        Some(Command::Completions(args)) => {
            run_completions(args.shell);
            Ok(())
        }
        None => start(cli),
    }
}

/// Where the setup instructions live.
///
/// Named in the error a first run produces, because that error is the first sentence
/// tui-do ever says to somebody who installed it from a package manager and has read
/// nothing. Pointing at the section beats describing it badly in one line.
const SETUP_URL: &str = "https://github.com/sjwasko/tui-do#required---first-time-setup";

/// What to say when the config cannot be loaded.
///
/// Two unrelated failures reach here as one error, because `Config::load` reports a
/// missing file and a malformed one the same way. They need opposite advice: a first run
/// has nothing and needs to be told how to write one, while a file that is already there
/// is a syntax or permission problem, and telling that person to go and create a config
/// they can plainly see is worse than saying nothing. So they are separated on
/// `exists()` rather than on the error.
///
/// Neither arm mentions `tui-do migrate`. It imports a cria config, which is a thing
/// almost nobody arriving here has, and it was the whole of what this error used to
/// suggest -- so a stranger's first encounter with tui-do was advice about a program
/// they have never heard of.
fn config_problem(path: &Path) -> String {
    if path.exists() {
        format!(
            "could not read {}. The file is there, so this is a permission or syntax \
             problem rather than a missing config -- unknown keys are rejected, so one \
             that was pasted into a terminal and arrived mangled fails exactly here.",
            path.display()
        )
    } else {
        format!(
            "no config at {}. Run `tui-do login` and it will write one, store your API \
             token privately, and check the token works before you need it. {SETUP_URL}",
            path.display()
        )
    }
}

/// Load the config and hand over to the interface.
///
/// The runtime is only started here, so `main` itself stays synchronous and every early
/// failure -- an unreadable config, for instance -- is reported to a terminal
/// that is still in its normal state.
fn start(cli: Cli) -> anyhow::Result<()> {
    let path = Config::resolve_path(cli.config.as_deref())?;
    let config = Config::load(&path).with_context(|| config_problem(&path))?;
    // After the config, so a first run without one is still told how to write one,
    // whatever it is attached to. Before the store, the runtime and the terminal, so a
    // refusal leaves nothing behind.
    interactive(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )
    .map_err(anyhow::Error::msg)?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?
        .block_on(runtime::run(config, path))
}

/// Which of stdin and stdout is not a terminal, in words, or `None` if both are.
fn missing_terminal(stdin_tty: bool, stdout_tty: bool) -> Option<&'static str> {
    match (stdin_tty, stdout_tty) {
        (true, true) => None,
        (false, true) => Some("stdin is not a terminal"),
        (true, false) => Some("stdout is not a terminal"),
        (false, false) => Some("neither stdin nor stdout is a terminal"),
    }
}

/// Whether the interface can start, given which of stdin and stdout are terminals.
///
/// Both have to be. Raw mode and the alternate screen only need stdout, so with stdin
/// redirected the interface used to come up anyway and wait for a key that could never
/// arrive -- and raw mode turns Ctrl-C into a byte, so nothing but a `kill` from another
/// terminal ended it (BUG-25). A stdout that is not a terminal has nowhere to draw.
fn interactive(stdin_tty: bool, stdout_tty: bool) -> Result<(), String> {
    missing_terminal(stdin_tty, stdout_tty).map_or(Ok(()), |what| {
        Err(format!(
            "{what}, and the interface needs one to read keys from and draw on. \
             To add a task from a script, use `tui-do add` (see `tui-do add --help`)."
        ))
    })
}

/// Whether `--edit` can open an editor: the same two terminals the interface needs,
/// for the same reason -- an editor started without them waits for keys that cannot
/// arrive, or draws where nobody is looking.
fn editable(stdin_tty: bool, stdout_tty: bool) -> Result<(), String> {
    missing_terminal(stdin_tty, stdout_tty).map_or(Ok(()), |what| {
        Err(format!(
            "{what}, and --edit opens an editor, which needs one. From a script, pass \
             the description with --description-file - or -d instead."
        ))
    })
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

/// Add a task from the command line, or, given no text at a terminal, ask for one.
fn run_add(args: &AddArgs, config_override: Option<&Path>) -> anyhow::Result<()> {
    if args.tokens {
        // Before the config: the names come from the store alone, and nothing is sent.
        let listing = runtime::store_only_runtime()?.block_on(runtime::tokens())?;
        print!("{listing}");
        return Ok(());
    }
    // First, so a script piping a description in without the text is told exactly that,
    // rather than clap's generic "TEXT is required".
    stdin_is_free(args)?;
    // Before the config, as clap's own refusal was: a script that forgot the text is told
    // so whatever state its config is in.
    if args.text.is_empty()
        && interactive(
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        )
        .is_err()
    {
        missing_text();
    }
    // Before anything is read or opened, so a script that asked for an editor is told
    // at once and nothing is queued.
    if args.edit {
        editable(
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        )
        .map_err(anyhow::Error::msg)?;
    }
    let description = read_description(args)?;
    let path = Config::resolve_path(config_override)?;
    let config = Config::load(&path).with_context(|| config_problem(&path))?;
    report_exposed_credentials(&config, &path, &mut std::io::stderr());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;
    let mode = add_mode(args, description);
    let text = if args.text.is_empty() {
        let vocabulary = runtime.block_on(runtime::vocabulary());
        // `--dry-run` with no text asks, then checks what was typed instead of adding it.
        match runtime::ask_for_task(runtime.handle(), vocabulary, mode.dry_run)? {
            Some(text) => text,
            None => bail!("cancelled; nothing was added"),
        }
    } else {
        args.text.join(" ")
    };
    // One call for both forms, so a typed task goes exactly where `add 'text'` would.
    runtime.block_on(runtime::add(&config, &path, &text, mode))
}

/// How `tui-do add` was asked to run: one value for the typed line and the given one, so
/// a prompt cannot add where `add 'text'` with the same flags would only have checked.
fn add_mode(args: &AddArgs, description: runtime::DescriptionSource) -> runtime::AddMode {
    runtime::AddMode {
        offline: args.offline,
        create_labels: args.create_labels,
        dry_run: args.dry_run,
        description,
    }
}

/// Exit with clap's error for a missing `TEXT`, exactly as when it was required.
///
/// The argument is optional so a terminal can be asked instead, which leaves clap with
/// nothing to say when there is no terminal. So this re-parses the real command line
/// with the argument required again, and lets clap say what it always said, with its
/// usage line and its exit code of 2.
fn missing_text() -> ! {
    let refused = Cli::command()
        .mut_subcommand("add", |add| add.mut_arg("text", |text| text.required(true)))
        .try_get_matches_from(std::env::args_os());
    match refused {
        Err(error) => error.exit(),
        // Unreachable while `run_add` only calls this with no text; say so rather than
        // carry on with nothing to add.
        Ok(_) => {
            eprintln!("error: tui-do add needs the task's text");
            std::process::exit(2)
        }
    }
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
        // `--config` and `--description-file` legitimately take a path; nothing else in
        // the arm may.
        let paths = arm.matches("compgen -f").count();
        assert_eq!(
            paths, 2,
            "only --config and --description-file complete a path:\n{arm}"
        );
    }

    fn add_args(argv: &[&str]) -> Result<AddArgs, clap::Error> {
        let mut full = vec!["tui-do", "add"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full)?.command {
            Some(Command::Add(args)) => Ok(args),
            other => panic!("expected add, got {other:?}"),
        }
    }

    #[test]
    fn an_inline_description_keeps_its_newlines_and_a_leading_hyphen() {
        let args = add_args(&["title", "-d", "- one\n- two\r\n"]).unwrap();
        assert_eq!(
            read_description(&args).unwrap(),
            // Byte for byte, as its help says: the trailing CRLF too.
            runtime::DescriptionSource::Given("- one\n- two\r\n".to_string())
        );
        let none = add_args(&["title"]).unwrap();
        assert_eq!(
            read_description(&none).unwrap(),
            runtime::DescriptionSource::default()
        );
    }

    #[test]
    fn the_inline_and_file_descriptions_conflict() {
        let error = add_args(&["title", "-d", "x", "--description-file", "f"])
            .expect_err("two descriptions must be refused");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn a_missing_description_file_is_named() {
        let args = add_args(&["title", "--description-file", "/nonexistent/plan.md"]).unwrap();
        let error = read_description(&args).unwrap_err();
        assert!(
            format!("{error:#}").contains("/nonexistent/plan.md"),
            "{error:#}"
        );
    }

    #[test]
    fn a_description_on_stdin_needs_the_text_as_an_argument() {
        let mut args = add_args(&["title", "--description-file", "-"]).unwrap();
        assert!(stdin_is_free(&args).is_ok());
        // No text means "ask for it" at a terminal, and stdin cannot be both the
        // description and the prompt's keyboard.
        args.text.clear();
        let error = stdin_is_free(&args).unwrap_err();
        assert!(
            error.to_string().contains("give the task's text"),
            "{error}"
        );

        let file = add_args(&["title", "--description-file", "plan.md"]).unwrap();
        let mut file = file;
        file.text.clear();
        assert!(stdin_is_free(&file).is_ok(), "only `-` reads stdin");
    }

    #[test]
    fn the_editor_conflicts_with_the_other_descriptions_and_a_dry_run() {
        for other in [
            &["-d", "x"][..],
            &["--description-file", "f"][..],
            &["--dry-run"][..],
        ] {
            let mut argv = vec!["title", "--edit"];
            argv.extend_from_slice(other);
            let error = add_args(&argv).expect_err("refused");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{other:?}"
            );
        }
        let args = add_args(&["title", "-e"]).unwrap();
        assert_eq!(
            read_description(&args).unwrap(),
            runtime::DescriptionSource::Edit
        );
    }

    #[test]
    fn the_editor_needs_a_terminal_at_both_ends() {
        assert_eq!(editable(true, true), Ok(()));
        for (stdin, stdout) in [(false, true), (true, false), (false, false)] {
            let why = editable(stdin, stdout).expect_err("no terminal, no editor");
            assert!(
                why.starts_with(missing_terminal(stdin, stdout).unwrap()),
                "{why}"
            );
            assert!(why.contains("--description-file -"), "{why}");
        }
    }

    #[test]
    fn reading_stops_one_byte_past_the_cap() {
        let max = tui_do_core::description::MAX_BYTES;
        let huge = std::io::repeat(b'a').take(u64::try_from(max).unwrap() * 4);
        use std::io::Read as _;
        assert_eq!(read_capped(huge).unwrap().len(), max + 1);
    }

    #[test]
    fn add_with_no_text_keeps_its_flags_for_the_line_it_asks_for() {
        let cli = Cli::try_parse_from(["tui-do", "add", "--dry-run", "--create-labels"]).unwrap();
        let Some(Command::Add(args)) = cli.command else {
            panic!("expected add");
        };
        assert!(args.text.is_empty(), "no text is allowed, to ask for one");
        let mode = add_mode(&args, runtime::DescriptionSource::default());
        assert!(mode.dry_run && mode.create_labels && !mode.offline);
    }

    #[test]
    fn the_interface_starts_only_with_a_terminal_at_both_ends() {
        assert_eq!(interactive(true, true), Ok(()));
        for (stdin, stdout, named) in [
            (false, true, "stdin is not a terminal"),
            (true, false, "stdout is not a terminal"),
            (false, false, "neither stdin nor stdout is a terminal"),
        ] {
            let why = interactive(stdin, stdout).expect_err("no terminal, no interface");
            assert!(why.starts_with(named), "{stdin}/{stdout} said: {why}");
            assert!(why.contains("`tui-do add`"), "no pointer to add: {why}");
        }
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
    fn a_missing_config_says_how_to_make_one() {
        let dir = scratch("missing-config");
        let path = dir.join("config.yaml");
        let message = config_problem(&path);

        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(
            message.contains(SETUP_URL),
            "it must point somewhere: {message}"
        );
        // It used to name `server.url` and leave the reader to write the file. There is a
        // command for that now, and naming it beats describing the YAML: the error a first
        // run hits should be one line the reader can act on, not a spec to implement.
        assert!(
            message.contains("tui-do login"),
            "send a first run to the command that fixes it: {message}"
        );
        // The whole point of the change. This error is the first thing a stranger who
        // installed from a package manager ever reads, and it used to recommend
        // importing a config from a program they have never heard of.
        assert!(
            !message.contains("migrate") && !message.contains("cria"),
            "a first run must not be sent to the cria importer: {message}"
        );
    }

    #[test]
    fn a_config_that_is_there_is_not_reported_as_missing() {
        let dir = scratch("malformed-config");
        let path = dir.join("config.yaml");
        std::fs::write(
            &path,
            "server:\n  url: https://example.invalid\n  nonsense: 1\n",
        )
        .unwrap();
        let message = config_problem(&path);

        // Telling someone to create a file they can see is worse than saying nothing,
        // so this arm must not offer the write-one-from-scratch advice.
        assert!(
            !message.contains("tui-do login"),
            "an existing file is not a missing one: {message}"
        );
        assert!(
            message.contains("mangled") || message.contains("syntax"),
            "say what kind of problem it actually is: {message}"
        );
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
