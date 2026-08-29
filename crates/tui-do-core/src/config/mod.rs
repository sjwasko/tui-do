//! Configuration: where it lives, what it holds, and how the API token is resolved.
//!
//! tui-do reads `~/.config/tui-do/config.yaml` (or `$XDG_CONFIG_HOME/tui-do/config.yaml`).
//! The schema is a clean break from cria's, and `tui-do migrate` imports the old one —
//! see [`migrate`].
//!
//! Two things are deliberately different from the file this is modelled on:
//!
//! **A broken config is an error, not a shrug.** cria's loader returns `Option` and maps
//! every failure — unreadable file, malformed YAML, wrong types — to `None`, which then
//! silently becomes the defaults. A mistyped key or a stray tab leaves the user pointed at
//! `https://vikunja.example.com` with no explanation. Every failure here carries the path
//! and the reason, and unknown keys are rejected rather than ignored, so a typo is caught
//! at the one moment the user can connect it to what they just edited.
//!
//! **Passwords are not configuration.** An API token can live in the file (or better, in a
//! file it points at); a password cannot. Logging in with one is interactive, and the JWT
//! that results is held in memory only.

pub mod columns;
pub mod migrate;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

pub use columns::{Column, ColumnLayout, ColumnSort, ColumnSpec, SortDirection};

/// Directory name under the user's config root.
const APP_DIR: &str = "tui-do";

/// File name within that directory.
const CONFIG_FILE: &str = "config.yaml";

/// Environment variable that overrides the config file location entirely.
pub const CONFIG_PATH_ENV: &str = "TUI_DO_CONFIG";

/// Environment variable that supplies the API token, overriding the file.
pub const API_TOKEN_ENV: &str = "TUI_DO_API_TOKEN";

/// Everything tui-do reads from disk at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Which server to talk to and how to authenticate.
    pub server: ServerConfig,

    /// How often to reconcile with the server.
    #[serde(default)]
    pub sync: SyncConfig,

    /// What the task list shows.
    #[serde(default)]
    pub view: ViewConfig,

    /// Single-key shortcuts for common edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quick_actions: Vec<QuickAction>,
}

/// Server address and credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Base URL of the Vikunja instance, with or without `/api/v1`.
    pub url: String,

    /// A scoped API token, inline.
    ///
    /// Convenient and the least private option: config files get copied into dotfile
    /// repos. Prefer [`ServerConfig::token_file`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    /// Path to a file whose first line is the API token.
    ///
    /// `~` is expanded; a relative path resolves against the config file's directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,

    /// Username to pre-fill on the login prompt. Never a password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// Background sync behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    /// Seconds between background pulls.
    pub interval_seconds: u64,

    /// Whether to pull in the background at all.
    pub enabled: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        // Five minutes, matching cria's default. The local store answers every read, so
        // this interval decides staleness, not responsiveness.
        Self {
            interval_seconds: 300,
            enabled: true,
        }
    }
}

/// What the task list shows on startup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewConfig {
    /// Where to start, and where a task with no project named goes.
    ///
    /// A title, or `#12` to name a project by id. Vikunja does not require titles to be
    /// unique -- an account seeded from another one can easily end up with two projects
    /// called `Inbox` -- and a title that two projects answer to picks whichever comes
    /// first. `#12` is the way to say which. `None` starts on everything and files a
    /// task with no project named in whatever is called `Inbox`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_project: Option<String>,

    /// Vikunja filter expression applied on startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_filter: Option<String>,

    /// Which of [`ViewConfig::layouts`] is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_layout: Option<String>,

    /// Named column layouts. Empty means use the built-in ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layouts: Vec<ColumnLayout>,
}

impl ViewConfig {
    /// The layouts to offer: the user's, or the built-in ones when they configured none.
    #[must_use]
    pub fn effective_layouts(&self) -> Vec<ColumnLayout> {
        if self.layouts.is_empty() {
            ColumnLayout::defaults()
        } else {
            self.layouts.clone()
        }
    }

    /// The layout to render with.
    ///
    /// Falls back to the first available rather than failing: a layout name that no
    /// longer exists should cost the user their preference, not their task list.
    #[must_use]
    pub fn active(&self) -> ColumnLayout {
        let layouts = self.effective_layouts();
        self.active_layout
            .as_deref()
            .and_then(|name| layouts.iter().find(|l| l.name == name).cloned())
            .or_else(|| layouts.first().cloned())
            .unwrap_or_else(|| ColumnLayout {
                name: "default".to_string(),
                description: None,
                columns: vec![ColumnSpec::new(Column::Title)],
            })
    }
}

/// What a quick action does when its key is pressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "target")]
pub enum QuickActionKind {
    /// Move the task to a project, by title.
    Project(String),
    /// Set priority, 1 to 5.
    Priority(u8),
    /// Toggle a label, by title.
    Label(String),
}

/// A single-key edit applied to the selected task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuickAction {
    /// The key that triggers it, pressed after the quick-action prefix.
    pub key: char,

    /// What it does.
    #[serde(flatten)]
    pub kind: QuickActionKind,
}

impl Config {
    /// The path tui-do will read, given an optional explicit override.
    ///
    /// Precedence: the `--config` argument, then `TUI_DO_CONFIG`, then the XDG location.
    ///
    /// # Errors
    /// [`CoreError::Config`] when no home or config directory can be determined, which
    /// is the one case with nothing sensible to fall back to.
    pub fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
        if let Some(path) = explicit {
            return Ok(path.to_path_buf());
        }
        if let Some(from_env) = std::env::var_os(CONFIG_PATH_ENV) {
            if !from_env.is_empty() {
                return Ok(PathBuf::from(from_env));
            }
        }
        Ok(Self::config_dir()?.join(CONFIG_FILE))
    }

    /// The directory tui-do keeps its config in.
    ///
    /// # Errors
    /// [`CoreError::Config`] when the platform reports no config directory.
    pub fn config_dir() -> Result<PathBuf> {
        dirs::config_dir()
            .map(|dir| dir.join(APP_DIR))
            .ok_or_else(|| CoreError::Config {
                path: "$XDG_CONFIG_HOME".to_string(),
                reason: "no config directory could be determined for this user".to_string(),
            })
    }

    /// Load the config from `path`.
    ///
    /// # Errors
    /// [`CoreError::Config`] if the file cannot be read, is not valid YAML, or contains a
    /// key tui-do does not recognise. Each carries the path and the underlying reason,
    /// because "it silently did nothing" is the failure this replaces.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| CoreError::Config {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        Self::from_yaml(&raw).map_err(|e| match e {
            CoreError::Config { reason, .. } => CoreError::Config {
                path: path.display().to_string(),
                reason,
            },
            other => other,
        })
    }

    /// Parse a config from YAML text.
    ///
    /// # Errors
    /// [`CoreError::Config`] describing what YAML rejected.
    pub fn from_yaml(raw: &str) -> Result<Self> {
        serde_yaml_ng::from_str(raw).map_err(|e| CoreError::Config {
            path: "<yaml>".to_string(),
            reason: e.to_string(),
        })
    }

    /// Render the config as YAML, for writing an example file.
    ///
    /// # Errors
    /// [`CoreError::Config`] if serialization fails, which would be a bug here rather
    /// than a user error.
    pub fn to_yaml(&self) -> Result<String> {
        serde_yaml_ng::to_string(self).map_err(|e| CoreError::Config {
            path: "<yaml>".to_string(),
            reason: e.to_string(),
        })
    }

    /// Write the config to `path`, creating parent directories.
    ///
    /// # Errors
    /// [`CoreError::Config`] on any I/O or serialization failure.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Config {
                path: parent.display().to_string(),
                reason: e.to_string(),
            })?;
            restrict_to_owner(parent);
        }
        let yaml = self.to_yaml()?;
        write_private(path, yaml.as_bytes()).map_err(|e| CoreError::Config {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
    }

    /// The API token, from the environment, a token file, or the config itself.
    ///
    /// `config_path` is used to resolve a relative `token_file`, so a config that ships
    /// with its token file beside it works wherever it is checked out to.
    ///
    /// # Errors
    /// [`CoreError::Config`] when both `token` and `token_file` are set — an ambiguity
    /// worth refusing rather than resolving by precedence nobody remembers — when the
    /// token file cannot be read, or when it is empty.
    pub fn api_token(&self, config_path: &Path) -> Result<Option<String>> {
        if let Some(from_env) = std::env::var(API_TOKEN_ENV).ok().filter(|t| !t.is_empty()) {
            return Ok(Some(from_env));
        }

        match (&self.server.token, &self.server.token_file) {
            (Some(_), Some(_)) => Err(CoreError::Config {
                path: config_path.display().to_string(),
                reason: "set either server.token or server.token_file, not both".to_string(),
            }),
            (Some(token), None) => {
                let token = token.trim();
                if token.is_empty() {
                    return Err(CoreError::Config {
                        path: config_path.display().to_string(),
                        reason: "server.token is empty".to_string(),
                    });
                }
                Ok(Some(token.to_string()))
            }
            (None, Some(file)) => {
                let resolved = resolve_relative(file, config_path);
                Ok(Some(read_token_file(&resolved)?))
            }
            (None, None) => Ok(None),
        }
    }

    /// An example config, for `tui-do init` and for the README.
    #[must_use]
    pub fn example() -> Self {
        Self {
            server: ServerConfig {
                url: "https://vikunja.example.com".to_string(),
                token: None,
                token_file: Some(PathBuf::from("~/.config/tui-do/token")),
                username: None,
            },
            sync: SyncConfig::default(),
            view: ViewConfig {
                default_filter: Some("done = false".to_string()),
                active_layout: Some("default".to_string()),
                ..ViewConfig::default()
            },
            quick_actions: vec![
                QuickAction {
                    key: 'u',
                    kind: QuickActionKind::Priority(5),
                },
                QuickAction {
                    key: 'w',
                    kind: QuickActionKind::Project("Work".to_string()),
                },
            ],
        }
    }
}

impl Config {
    /// Credential-bearing files that other accounts on this machine can read.
    ///
    /// Returns a description per file, ready to show. The config itself counts whenever
    /// it carries an inline `server.token`; a `token_file` counts always, since it holds
    /// nothing but the credential.
    ///
    /// This exists as a return value rather than a `tracing::warn!` because a warning the
    /// user never sees is not a warning. The log line that used to be the only mechanism
    /// went to a subscriber that was never installed, and even with one installed the
    /// interface owns the terminal — so the finding has to reach the model to be worth
    /// making.
    #[must_use]
    pub fn exposed_credential_files(&self, config_path: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut check = |path: &Path| {
            if world_readable(path) {
                found.push(format!(
                    "{} is readable by other users; chmod 600 it",
                    path.display()
                ));
            }
        };

        if self.server.token.is_some() {
            check(config_path);
        }
        if let Some(file) = &self.server.token_file {
            check(&resolve_relative(file, config_path));
        }
        found
    }
}

/// Whether anyone but the owner can read this path.
#[cfg(unix)]
fn world_readable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
}

/// Unknown on platforms without Unix permission bits, which is reported as "fine".
#[cfg(not(unix))]
fn world_readable(_path: &Path) -> bool {
    false
}

/// Expand `~` and resolve a relative path against the config file's directory.
fn resolve_relative(path: &Path, config_path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match config_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(path),
        _ => path.to_path_buf(),
    }
}

/// Read a token from its own file, warning if anyone else can read it.
fn read_token_file(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path).map_err(|e| CoreError::Config {
        path: path.display().to_string(),
        reason: format!("could not read the API token file: {e}"),
    })?;

    warn_if_world_readable(path);

    let token = raw.lines().next().unwrap_or_default().trim();
    if token.is_empty() {
        return Err(CoreError::Config {
            path: path.display().to_string(),
            reason: "the API token file is empty".to_string(),
        });
    }
    Ok(token.to_string())
}

/// Write a file only its owner can read.
///
/// `std::fs::write` creates at `0o666 & !umask`, which on a default umask is `0o644` —
/// world-readable. The config carries `server.token` whenever `tui-do migrate` copies one
/// out of a cria config, so the plain call handed the user's API token to every account on
/// the machine and then printed advice about dotfile repositories. The mode is set at
/// creation rather than afterwards: a `chmod` after the fact leaves a window in which the
/// token is on disk and readable.
///
/// Applied unconditionally rather than only when a token is present, so that adding one
/// later cannot quietly land in a file that was created permissive.
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    // An existing file keeps the mode it already had, so tighten it too.
    restrict_to_owner(path);
    Ok(())
}

/// Take group and other permissions off a path, best-effort.
///
/// Silent on failure: this hardens a path that has already been written, and a filesystem
/// that cannot express the mode is not a reason to fail the write.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode();
    // Directories need the owner's execute bit to be traversable; files do not get it.
    let wanted = if metadata.is_dir() { 0o700 } else { 0o600 };
    if mode & 0o077 != 0 {
        let mut permissions = metadata.permissions();
        permissions.set_mode(wanted);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

/// No-op on platforms without Unix permission bits.
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) {}

/// Say something when a credential file is readable by more than its owner.
///
/// A warning rather than a refusal: it is the user's machine and their call, and refusing
/// to start over a permission bit would be its own kind of rude. But a token file at 0644
/// in a dotfiles repo is worth one line of output.
#[cfg(unix)]
fn warn_if_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o077;
    if mode != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{:o}", metadata.permissions().mode() & 0o777),
            "the API token file is readable by other users; consider chmod 600"
        );
    }
}

/// No-op on platforms without Unix permission bits.
#[cfg(not(unix))]
fn warn_if_world_readable(_path: &Path) {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn minimal() -> Config {
        Config {
            server: ServerConfig {
                url: "https://vikunja.example".to_string(),
                token: None,
                token_file: None,
                username: None,
            },
            sync: SyncConfig::default(),
            view: ViewConfig::default(),
            quick_actions: Vec::new(),
        }
    }

    #[test]
    fn a_minimal_config_needs_only_a_url() {
        let config = Config::from_yaml("server:\n  url: https://vikunja.example\n").unwrap();
        assert_eq!(config.server.url, "https://vikunja.example");
        assert_eq!(config.sync.interval_seconds, 300);
        assert!(config.sync.enabled);
    }

    #[test]
    fn a_missing_url_is_reported_rather_than_defaulted() {
        // cria defaults to https://vikunja.example.com, so a config with no url produces
        // a client pointed at a domain that is not the user's, and a confusing failure.
        let err = Config::from_yaml("sync:\n  enabled: false\n  interval_seconds: 60\n")
            .expect_err("a config with no server should not load");
        assert!(matches!(err, CoreError::Config { .. }));
    }

    #[test]
    fn a_typo_in_a_key_is_an_error() {
        let err = Config::from_yaml("server:\n  url: https://x\n  tokenn: abc\n")
            .expect_err("unknown keys should be rejected");
        let CoreError::Config { reason, .. } = err else {
            panic!("expected a config error");
        };
        assert!(reason.contains("tokenn"), "unhelpful message: {reason}");
    }

    #[test]
    fn malformed_yaml_says_so() {
        let err = Config::from_yaml("server:\n\turl: tabs are not yaml\n").expect_err("bad yaml");
        assert!(matches!(err, CoreError::Config { .. }));
    }

    #[test]
    fn a_config_round_trips_through_yaml() {
        let config = Config::example();
        let parsed = Config::from_yaml(&config.to_yaml().unwrap()).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn setting_both_token_and_token_file_is_refused() {
        let mut config = minimal();
        config.server.token = Some("tk_inline".to_string());
        config.server.token_file = Some(PathBuf::from("/tmp/token"));
        let err = config
            .api_token(Path::new("/tmp/config.yaml"))
            .expect_err("ambiguous credential config should be refused");
        let CoreError::Config { reason, .. } = err else {
            panic!("expected a config error");
        };
        assert!(reason.contains("not both"));
    }

    #[test]
    fn an_inline_token_is_returned_and_trimmed() {
        let mut config = minimal();
        config.server.token = Some("  tk_inline\n".to_string());
        assert_eq!(
            config.api_token(Path::new("/tmp/config.yaml")).unwrap(),
            Some("tk_inline".to_string())
        );
    }

    #[test]
    fn no_credential_configured_is_not_an_error() {
        // Logging in with a password is a valid way to run, so an absent token is a
        // state to handle, not a failure.
        assert_eq!(
            minimal().api_token(Path::new("/tmp/config.yaml")).unwrap(),
            None
        );
    }

    #[test]
    fn a_relative_token_file_resolves_against_the_config_directory() {
        let resolved = resolve_relative(
            Path::new("token"),
            Path::new("/home/someone/.config/tui-do/config.yaml"),
        );
        assert_eq!(
            resolved,
            PathBuf::from("/home/someone/.config/tui-do/token")
        );
    }

    #[test]
    fn an_absolute_token_file_is_left_alone() {
        let resolved = resolve_relative(Path::new("/etc/tui-do/token"), Path::new("/tmp/c.yaml"));
        assert_eq!(resolved, PathBuf::from("/etc/tui-do/token"));
    }

    #[test]
    fn a_tilde_token_file_expands_to_the_home_directory() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let resolved = resolve_relative(Path::new("~/secrets/token"), Path::new("/tmp/c.yaml"));
        assert_eq!(resolved, home.join("secrets/token"));
    }

    #[test]
    fn an_explicit_path_wins_over_everything() {
        let explicit = PathBuf::from("/tmp/explicit.yaml");
        assert_eq!(
            Config::resolve_path(Some(&explicit)).unwrap(),
            explicit,
            "--config should not be overridable by the environment"
        );
    }

    #[test]
    fn the_active_layout_falls_back_rather_than_failing() {
        let view = ViewConfig {
            active_layout: Some("a layout that was deleted".to_string()),
            ..ViewConfig::default()
        };
        // Losing the preference is acceptable; losing the task list is not.
        assert_eq!(view.active().name, "default");
    }

    #[test]
    fn configured_layouts_replace_the_built_in_ones() {
        let view = ViewConfig {
            layouts: vec![ColumnLayout {
                name: "mine".to_string(),
                description: None,
                columns: vec![ColumnSpec::new(Column::Title)],
            }],
            ..ViewConfig::default()
        };
        assert_eq!(view.effective_layouts().len(), 1);
        assert_eq!(view.active().name, "mine");
    }

    #[test]
    fn quick_actions_parse_as_a_tagged_shape() {
        let yaml = "\
server:
  url: https://vikunja.example
quick_actions:
  - key: u
    action: priority
    target: 5
  - key: w
    action: project
    target: Work
";
        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.quick_actions.len(), 2);
        assert_eq!(config.quick_actions[0].key, 'u');
        assert_eq!(config.quick_actions[0].kind, QuickActionKind::Priority(5));
        assert_eq!(
            config.quick_actions[1].kind,
            QuickActionKind::Project("Work".to_string())
        );
    }

    #[test]
    fn an_unknown_quick_action_is_rejected() {
        let yaml = "\
server:
  url: https://vikunja.example
quick_actions:
  - key: x
    action: teleport
    target: away
";
        assert!(Config::from_yaml(yaml).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_saved_config_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        // `std::fs::write` creates at 0644. The config carries `server.token` whenever
        // `tui-do migrate` copies one out of a cria config, so the plain call published
        // the user's API token to every account on the machine.
        let dir = std::env::temp_dir().join(format!("tui-do-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");

        let mut config = minimal();
        config.server.token = Some("tk_secret".to_string());
        config.save(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "written as {mode:o}");
        assert_eq!(config.exposed_credential_files(&path), Vec::<String>::new());

        // And a file that was already permissive is reported rather than ignored.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let exposed = config.exposed_credential_files(&path);
        assert_eq!(exposed.len(), 1, "{exposed:?}");
        assert!(exposed[0].contains("chmod 600"));

        // A config with no inline token has nothing of its own to expose.
        let mut tokenless = minimal();
        tokenless.server.token = None;
        assert!(tokenless.exposed_credential_files(&path).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
