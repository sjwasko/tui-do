//! `GET /info` — what the server says about itself.

use serde::{Deserialize, Serialize};

/// The page size to assume before `/info` has been read.
///
/// Vikunja's own default, and the value both our servers report. It is a starting point,
/// never a conclusion: the real cap replaces it as soon as `/info` answers, and the
/// pagination headers are believed over both.
pub const DEFAULT_MAX_ITEMS_PER_PAGE: u32 = 50;

/// Server version, limits, and which optional features are switched on.
///
/// Read at startup. `max_items_per_page` is the field that matters most: it is where the
/// pagination cap comes from, instead of the hardcoded `per_page=10000` that silently
/// truncated cria's task list at 50.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Version string, e.g. `v2.5.0`.
    #[serde(default)]
    pub version: String,

    /// The server's hard cap on `per_page`. `0` when the server did not say.
    #[serde(default)]
    pub max_items_per_page: u32,

    /// Where the web UI lives, used to build "open in browser" links.
    #[serde(default)]
    pub frontend_url: String,

    /// Message of the day, shown by the web UI on login.
    #[serde(default)]
    pub motd: String,

    /// Largest accepted attachment, as a human string such as `20MB`.
    #[serde(default)]
    pub max_file_size: String,

    /// Import formats this server accepts.
    #[serde(default, deserialize_with = "crate::models::nullable::null_as_default")]
    pub available_migrators: Vec<String>,

    /// Whether task comments are enabled.
    #[serde(default)]
    pub task_comments_enabled: bool,

    /// Whether task attachments are enabled.
    #[serde(default)]
    pub task_attachments_enabled: bool,

    /// Whether CalDAV is served.
    #[serde(default)]
    pub caldav_enabled: bool,

    /// Whether the server's database tolerates concurrent writes.
    ///
    /// False on SQLite, where overlapping write transactions deadlock. The sync engine
    /// serialises its outbox instead of firing writes in parallel when this is false —
    /// our servers are Postgres, so it is true, but the flag exists precisely because it
    /// is not always.
    #[serde(default)]
    pub concurrent_writes: bool,

    /// Whether TOTP is available, which decides whether to offer a passcode prompt.
    #[serde(default)]
    pub totp_enabled: bool,

    /// Whether webhooks are enabled.
    #[serde(default)]
    pub webhooks_enabled: bool,

    /// Whether link sharing is enabled.
    #[serde(default)]
    pub link_sharing_enabled: bool,

    /// Whether this is a demo instance, where data is wiped periodically.
    #[serde(default)]
    pub demo_mode_enabled: bool,

    /// Whether email reminders are configured.
    #[serde(default)]
    pub email_reminders_enabled: bool,
}

impl ServerInfo {
    /// The largest `per_page` worth requesting.
    ///
    /// Falls back to [`DEFAULT_MAX_ITEMS_PER_PAGE`] when the server reports `0`, which is
    /// what an older or misconfigured instance does.
    #[must_use]
    pub fn page_cap(&self) -> u32 {
        if self.max_items_per_page == 0 {
            DEFAULT_MAX_ITEMS_PER_PAGE
        } else {
            self.max_items_per_page
        }
    }

    /// Whether the server can import the export format `seed-from-prod.sh` produces.
    #[must_use]
    pub fn supports_vikunja_file_migration(&self) -> bool {
        self.available_migrators.iter().any(|m| m == "vikunja-file")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Trimmed from the real `GET /info` response of the dev instance.
    const LIVE: &str = r#"{
        "version": "v2.5.0",
        "max_items_per_page": 50,
        "frontend_url": "https://vikunja.example.com/",
        "concurrent_writes": true,
        "available_migrators": ["vikunja-file", "ticktick", "wekan", "csv"],
        "task_comments_enabled": true,
        "auth": {"local": {"enabled": true}},
        "legal": {"imprint_url": "", "privacy_policy_url": ""}
    }"#;

    #[test]
    fn parses_the_live_response_and_ignores_what_it_does_not_model() {
        let info: ServerInfo = serde_json::from_str(LIVE).unwrap();
        assert_eq!(info.version, "v2.5.0");
        assert_eq!(info.page_cap(), 50);
        assert!(info.concurrent_writes);
        assert!(info.supports_vikunja_file_migration());
    }

    #[test]
    fn a_missing_cap_falls_back_rather_than_meaning_unlimited() {
        // Zero must not be read as "ask for everything at once" -- that is cria's bug.
        let info = ServerInfo::default();
        assert_eq!(info.page_cap(), DEFAULT_MAX_ITEMS_PER_PAGE);
    }
}
