//! `tui-do login` — write the config, store the token, and prove it works.
//!
//! This exists to delete three of the six steps the README's *REQUIRED — First time setup*
//! asks for: the `printf` that creates the token file, the `chmod 600` that makes it
//! private, and the `$EDITOR config.yaml` that writes the config by hand. Those are the
//! least Mac-shaped instructions tui-do gives, and they are the first thing a
//! `brew install` user meets — but they are no more pleasant on a Pi, which is why this is
//! one command on both platforms rather than a macOS convenience. Only *where the token
//! lands* differs by platform, and that is the seam `CLAUDE.md` already describes.
//!
//! **It is also where OAuth lands later.** The paste step disappears and everything else a
//! user learned about this command stays true, which is the whole reason it was named and
//! shaped before the storage question was settled.
//!
//! ## Why it checks the token before writing it
//!
//! `GET /user` is the one permission the README says tui-do *does not start* without, and
//! a token missing it fails in the most confusing way the project has: every task and
//! project call succeeds, and the user sees a red toast reading "not authorized: missing,
//! malformed, expired or otherwise invalid token provided" — which reads like the token is
//! wrong when it is merely incomplete. Asking `GET /user` here turns that into a sentence
//! at the moment the person is looking at the permission checkboxes.
//!
//! It catches the other half too. `CLAUDE.md` records, twice, credentials arriving mangled
//! from a paste; a token that lost a character is rejected here rather than at the next
//! launch.

use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{bail, Context};
use tui_do_core::config::{write_token_file, Config, ServerConfig, SyncConfig, TOKEN_FILE};

/// Vikunja's API-token page, relative to the instance root.
///
/// **This is a front-end route and cannot be verified by asking the server.** Vikunja's web
/// interface is a single-page application, so it answers the same `index.html` for every
/// path it owns, real or invented — `md/2026-09-07-credential-storage-design.md` records a
/// whole wrong conclusion reached by treating a `200` from that catch-all as evidence a
/// route existed. A probe here would prove nothing, so the breadcrumb is printed beside the
/// link: if the deep link is ever wrong, *Settings → API tokens* still gets the user there.
const TOKEN_PAGE: &str = "user/settings/api-tokens";

/// Where the token permissions are explained.
const PERMISSIONS_URL: &str = "https://github.com/sjwasko/tui-do#token-permissions";

/// Run the login flow.
pub(crate) fn run(
    url_arg: Option<&str>,
    no_browser: bool,
    config_override: Option<&Path>,
) -> anyhow::Result<()> {
    let path = Config::resolve_path(config_override)?;

    // An existing config is read and kept, never replaced. Logging in again is an ordinary
    // thing to do — a token was rotated, a server moved — and someone who has tuned their
    // columns and quick actions should not lose them to it. A file that is there but will
    // not parse stops the command instead: overwriting it would destroy settings the user
    // cannot get back, and `deny_unknown_fields` means a mangled paste lands exactly here.
    let existing = if path.exists() {
        Some(Config::load(&path).with_context(|| {
            format!(
                "{} exists but could not be read, so login will not overwrite it. Fix or \
                 move it aside first.",
                path.display()
            )
        })?)
    } else {
        None
    };

    // Piped stdin is the token and nothing else. Without this the URL prompt reads the
    // first line — the token — as the server, and the token read then hits end-of-input, so
    // the command fails with "no token given" while pointing at a server nobody typed.
    // Found driving `tui-do login < token` on 2026-09-14; the failure is confusing in
    // exactly the way this command exists to stop.
    if url_arg.is_none() && !std::io::stdin().is_terminal() {
        bail!(
            "stdin is not a terminal, so it is read as the token — pass --url as well:\n  \
             tui-do login --url vikunja.example.com < token"
        );
    }

    let default_url = existing.as_ref().map(|config| config.server.url.clone());
    let url = match url_arg {
        Some(given) => normalise_url(given)?,
        None => {
            let answer = ask("Server URL", default_url.as_deref())?;
            normalise_url(&answer)?
        }
    };

    let page = format!("{url}/{TOKEN_PAGE}");
    println!();
    println!("Create an API token in Vikunja, under Settings → API tokens:");
    println!("  {page}");
    println!();
    println!("It needs these permission groups, and `other → user` is the one that stops");
    println!("tui-do dead if it is missing — see {PERMISSIONS_URL}");
    println!("  other → user, tasks, tasks_labels, labels, projects, projects_views");
    println!();

    if !no_browser {
        open_browser(&page);
    }

    let token = ask_hidden("Paste the token")?;
    if token.is_empty() {
        bail!("no token given, so nothing was written.");
    }

    // Check before writing. Writing first would leave a broken credential on disk and make
    // the next launch the place the mistake shows up, which is the failure this command is
    // here to remove.
    let who = verify(&url, &token)?;
    println!("Signed in as {who}.");

    let token_path = path.parent().map_or_else(
        || Path::new(TOKEN_FILE).to_path_buf(),
        |dir| dir.join(TOKEN_FILE),
    );
    write_token_file(&token_path, &token)?;

    let (config, dropped_inline_token) = configure(existing, url);
    if dropped_inline_token {
        println!(
            "Removed the inline server.token from the config; the token is in {} now.",
            token_path.display()
        );
    }
    config.save(&path)?;

    println!();
    println!("Wrote {}", path.display());
    println!("Wrote {} (owner-readable only)", token_path.display());
    println!("Run `tui-do` to start.");
    Ok(())
}

/// Build the config to write, from the one already there if there is one.
///
/// Separated from [`run`] because it is the only part of this command with a decision in
/// it, and the rest is a terminal — this is testable and a prompt is not.
///
/// Returns the config and whether an inline `server.token` was dropped, which the caller
/// reports. Dropping it is not optional: [`Config::api_token`] refuses a config carrying
/// both `token` and `token_file` rather than resolving a precedence nobody remembers, so
/// leaving it would write a config that cannot start.
fn configure(existing: Option<Config>, url: String) -> (Config, bool) {
    let mut config = existing.unwrap_or_else(|| Config {
        server: ServerConfig {
            url: String::new(),
            token: None,
            token_file: None,
            username: None,
        },
        sync: SyncConfig::default(),
        view: tui_do_core::config::ViewConfig::default(),
        quick_actions: Vec::new(),
    });
    config.server.url = url;
    // Relative, so the pair travels: a config and its token copied to another box resolve
    // against wherever they landed rather than against this machine's home directory.
    config.server.token_file = Some(Path::new(TOKEN_FILE).to_path_buf());
    let dropped = config.server.token.take().is_some();
    (config, dropped)
}

/// Ask for a line, offering `default` when there is one.
///
/// An empty answer takes the default. With no default, an empty answer is refused rather
/// than accepted as a server URL of "", which fails much later and less clearly.
fn ask(prompt: &str, default: Option<&str>) -> anyhow::Result<String> {
    loop {
        match default {
            Some(value) => print!("{prompt} [{value}]: "),
            None => print!("{prompt}: "),
        }
        std::io::stdout()
            .flush()
            .context("could not write the prompt")?;

        let mut line = String::new();
        let read = std::io::stdin()
            .read_line(&mut line)
            .context("could not read your answer")?;
        if read == 0 {
            bail!("input ended before an answer was given.");
        }
        let line = line.trim();
        if !line.is_empty() {
            return Ok(line.to_string());
        }
        if let Some(value) = default {
            return Ok(value.to_string());
        }
        println!("  an answer is needed here.");
    }
}

/// Ask for a secret, without echoing it.
///
/// Raw mode rather than a new dependency: `crossterm` is already here for the interface,
/// and reading key events is the same mechanism the render loop uses. A bracketed paste
/// arrives as one `Paste` event and an unbracketed one as a run of `Char`s, so both are
/// handled — a pasted token is the *expected* input, not the exotic case.
///
/// When stdin is not a terminal there is nothing to echo to and nothing to put in raw
/// mode, so the token is read as a plain line. That is what makes the command scriptable,
/// and it is deliberate rather than a fallback: `tui-do login --url … < token` is a
/// reasonable thing to want on a box being provisioned.
fn ask_hidden(prompt: &str) -> anyhow::Result<String> {
    if !std::io::stdin().is_terminal() {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("could not read the token from stdin")?;
        return Ok(line.trim().to_string());
    }

    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

    print!("{prompt}: ");
    std::io::stdout()
        .flush()
        .context("could not write the prompt")?;

    crossterm::terminal::enable_raw_mode().context("could not read without echoing")?;
    let outcome = read_secret();
    // Restore the terminal whatever happened, before anything is printed into it.
    let restored = crossterm::terminal::disable_raw_mode();
    println!();
    restored.context("could not restore the terminal after reading the token")?;
    return outcome;

    /// The read itself, factored out so raw mode is disabled on every path out of it.
    fn read_secret() -> anyhow::Result<String> {
        let mut token = String::new();
        loop {
            match event::read().context("could not read a keystroke")? {
                Event::Paste(text) => token.push_str(text.trim()),
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Enter => return Ok(token.trim().to_string()),
                    KeyCode::Backspace => {
                        token.pop();
                    }
                    // Ctrl-C in raw mode is a keystroke rather than a signal, so it has to
                    // be answered here or the prompt cannot be escaped at all.
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        bail!("cancelled.")
                    }
                    KeyCode::Esc => bail!("cancelled."),
                    KeyCode::Char(c) => token.push(c),
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// Accept what a person would type and turn it into a base URL.
///
/// A bare host is the common case — people copy `vikunja.example.com` out of a browser's
/// address bar without the scheme — and refusing it would be pedantry. `https` is assumed
/// rather than `http`, since the alternative silently downgrades somebody's credential onto
/// a cleartext connection.
fn normalise_url(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("a server URL is needed.");
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    let parsed = url::Url::parse(&with_scheme)
        .with_context(|| format!("{raw} is not a URL tui-do can use"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => bail!("{other} is not a scheme tui-do can use; give an http or https URL."),
    }
    if parsed.host_str().is_none() {
        bail!("{raw} names no host.");
    }
    Ok(with_scheme.trim_end_matches('/').to_string())
}

/// Ask the server who this token belongs to.
///
/// Returns the display name, falling back to the login name — `User::name` is frequently
/// empty, which the model already documents.
fn verify(url: &str, token: &str) -> anyhow::Result<String> {
    let client = tui_do_api::Client::builder(url)
        .credentials(tui_do_api::Credentials::api_token(token.to_string()))
        .build()
        .with_context(|| format!("{url} is unusable"))?;

    let user = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?
        .block_on(client.current_user())
        .with_context(|| {
            format!(
                "the token was refused. If it is newly created, check it carries \
                 `other → user` — see {PERMISSIONS_URL}"
            )
        })?;

    Ok(if user.name.trim().is_empty() {
        user.username
    } else {
        user.name
    })
}

/// Hand the token page to the desktop, if there is one to hand it to.
///
/// Best-effort by design: failing to open a browser is not a reason to stop, because the
/// URL was printed a moment ago and the user can open it themselves. `xdg-open` over SSH
/// either fails or opens a browser on the machine at the far end of the connection, which
/// is not where the person is sitting — so the same environment check the interface uses
/// for `o` decides whether to try at all.
fn open_browser(page: &str) {
    if !can_open() {
        return;
    }
    match std::process::Command::new(crate::runtime::URL_OPENER)
        .arg(page)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => println!("Opening that page in your browser…"),
        Err(error) => println!("(could not open a browser: {error})"),
    }
}

/// Whether there is a local desktop to open a URL onto.
fn can_open() -> bool {
    matches!(
        crate::runtime::url_action(),
        tui_do_ui::model::UrlAction::Open
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;

    /// People copy a host out of a browser's address bar without the scheme, and refusing
    /// that would be pedantry. `https` rather than `http`, because guessing the other way
    /// puts a credential on a cleartext connection to save five characters.
    #[test]
    fn a_bare_host_is_assumed_to_be_https() {
        assert_eq!(
            normalise_url("vikunja.example.com").unwrap(),
            "https://vikunja.example.com"
        );
    }

    /// The base URL is joined to paths elsewhere, so a trailing slash would produce `//`.
    #[test]
    fn a_trailing_slash_is_taken_off() {
        assert_eq!(
            normalise_url("https://vikunja.example.com/").unwrap(),
            "https://vikunja.example.com"
        );
    }

    /// An explicit scheme is honoured, including `http`: a homelab on a trusted LAN is a
    /// real deployment, and this command is not the place to refuse one.
    #[test]
    fn an_explicit_scheme_is_kept() {
        assert_eq!(
            normalise_url("http://box.lan:3456").unwrap(),
            "http://box.lan:3456"
        );
    }

    #[test]
    fn a_scheme_that_is_not_http_is_refused() {
        assert!(normalise_url("ftp://vikunja.example.com").is_err());
        assert!(normalise_url("   ").is_err());
    }

    /// Logging in again is ordinary — a rotated token, a moved server — and it must not
    /// cost the user the settings they have tuned. Only the server fields move.
    #[test]
    fn logging_in_again_keeps_everything_that_is_not_the_server() {
        let mut existing = Config::example();
        existing.server.url = "https://old.example.com".to_string();
        existing.sync.interval_seconds = 42;
        let quick_actions = existing.quick_actions.clone();
        let view = existing.view.clone();

        let (config, dropped) = configure(Some(existing), "https://new.example.com".to_string());

        assert_eq!(config.server.url, "https://new.example.com");
        assert_eq!(config.sync.interval_seconds, 42);
        assert_eq!(config.quick_actions, quick_actions);
        assert_eq!(config.view, view);
        assert!(!dropped, "the example config carries no inline token");
    }

    /// `Config::api_token` refuses a config holding both, so login has to take the inline
    /// one out — otherwise it would write a config that cannot start, which is a worse
    /// first run than the one it replaced.
    #[test]
    fn an_inline_token_is_dropped_in_favour_of_the_file() {
        let mut existing = Config::example();
        existing.server.token = Some("tk_inline".to_string());
        existing.server.token_file = None;

        let (config, dropped) = configure(Some(existing), "https://example.com".to_string());

        assert!(dropped, "the caller has to be told, so it can say so");
        assert_eq!(config.server.token, None);
        assert_eq!(
            config.server.token_file.as_deref(),
            Some(Path::new("token"))
        );
        assert!(
            config.api_token(Path::new("/nowhere/config.yaml")).is_err(),
            "only because the file does not exist -- the both-set refusal is gone"
        );
    }

    /// A first run has no config at all, which is the case the whole command exists for.
    #[test]
    fn a_first_run_writes_a_relative_token_file() {
        let (config, dropped) = configure(None, "https://example.com".to_string());

        assert!(!dropped);
        assert_eq!(config.server.url, "https://example.com");
        assert_eq!(
            config.server.token_file.as_deref(),
            Some(Path::new("token"))
        );
        assert_eq!(
            config.sync,
            SyncConfig::default(),
            "a config written for someone should carry the ordinary defaults"
        );
    }
}
