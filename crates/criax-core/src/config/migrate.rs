//! Importing a cria config.
//!
//! criax's schema is a clean break, so this is a one-way translation rather than a
//! compatibility layer: read `~/.config/cria/config.yaml`, produce a [`Config`], and
//! report what did not carry across.
//!
//! It is deliberately lenient in a way [`Config::load`] is not. A cria config is
//! *someone else's* file — it may contain keys criax has never heard of, or keys cria
//! itself stopped using — and refusing to import over one of them would help nobody.
//! Unknown keys are collected and reported instead of rejected.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::columns::{Column, ColumnLayout, ColumnSort, ColumnSpec, SortDirection};
use super::{Config, QuickAction, QuickActionKind, ServerConfig, SyncConfig, ViewConfig};
use crate::error::{CoreError, Result};

/// Where cria keeps its config.
///
/// # Errors
/// [`CoreError::Config`] when no config directory can be determined.
pub fn cria_config_path() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|dir| dir.join("cria").join("config.yaml"))
        .ok_or_else(|| CoreError::Config {
            path: "$XDG_CONFIG_HOME".to_string(),
            reason: "no config directory could be determined for this user".to_string(),
        })
}

/// What an import produced, and what it could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// The translated config.
    pub config: Config,

    /// Human-readable notes about anything dropped or changed.
    ///
    /// Shown to the user after `criax migrate`, because a silent partial import is how
    /// someone discovers three weeks later that their quick actions are gone.
    pub notes: Vec<String>,
}

/// Read and translate a cria config.
///
/// # Errors
/// [`CoreError::Config`] if the file cannot be read or is not valid YAML.
pub fn from_cria_file(path: &Path) -> Result<Migration> {
    let raw = std::fs::read_to_string(path).map_err(|e| CoreError::Config {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    from_cria_yaml(&raw).map_err(|e| match e {
        CoreError::Config { reason, .. } => CoreError::Config {
            path: path.display().to_string(),
            reason,
        },
        other => other,
    })
}

/// Translate a cria config given as YAML text.
///
/// # Errors
/// [`CoreError::Config`] if the YAML does not parse.
pub fn from_cria_yaml(raw: &str) -> Result<Migration> {
    let old: CriaConfig = serde_yaml_ng::from_str(raw).map_err(|e| CoreError::Config {
        path: "<cria config>".to_string(),
        reason: e.to_string(),
    })?;
    Ok(old.into_migration())
}

/// cria's config schema, as much of it as is worth carrying across.
#[derive(Debug, Deserialize)]
struct CriaConfig {
    #[serde(default)]
    api_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_file: Option<String>,
    #[serde(default)]
    default_project: Option<String>,
    #[serde(default)]
    default_filter: Option<String>,
    #[serde(default)]
    quick_actions: Option<Vec<CriaQuickAction>>,
    #[serde(default)]
    column_layouts: Option<Vec<CriaColumnLayout>>,
    #[serde(default)]
    table_columns: Option<Vec<CriaTableColumn>>,
    #[serde(default)]
    active_layout: Option<String>,
    #[serde(default)]
    refresh_interval_seconds: Option<u64>,
    #[serde(default)]
    auto_refresh: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CriaQuickAction {
    key: String,
    action: String,
    target: String,
}

#[derive(Debug, Deserialize)]
struct CriaColumnLayout {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    columns: Vec<CriaTableColumn>,
}

#[derive(Debug, Deserialize)]
struct CriaTableColumn {
    #[serde(default)]
    name: Option<String>,
    column_type: String,
    #[serde(default)]
    width_percentage: Option<u16>,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    min_width: Option<u16>,
    #[serde(default)]
    max_width: Option<u16>,
    #[serde(default)]
    wrap_text: Option<bool>,
    #[serde(default)]
    sort: Option<CriaColumnSort>,
}

#[derive(Debug, Deserialize)]
struct CriaColumnSort {
    order: u16,
    #[serde(default)]
    direction: Option<String>,
}

const fn yes() -> bool {
    true
}

impl CriaConfig {
    fn into_migration(self) -> Migration {
        let mut notes = Vec::new();

        // cria's api_url usually already ends in /api/v1; criax's client accepts either,
        // but storing the bare server URL is what the rest of the config expects.
        let url = match self.api_url {
            Some(url) => {
                let trimmed = url.trim().trim_end_matches('/');
                trimmed
                    .strip_suffix("/api/v1")
                    .unwrap_or(trimmed)
                    .to_string()
            }
            None => {
                notes.push(
                    "the cria config had no api_url; set server.url before starting".to_string(),
                );
                String::new()
            }
        };

        // An inline key is carried across as-is, but say so: it is worth a nudge toward
        // the file form now that the user is editing this config anyway.
        let token = self.api_key.filter(|k| !k.trim().is_empty());
        let token_file = self
            .api_key_file
            .filter(|f| !f.trim().is_empty())
            .map(PathBuf::from);
        if token.is_some() && token_file.is_some() {
            notes.push(
                "cria had both api_key and api_key_file set; keeping api_key_file, since \
                 criax refuses both at once"
                    .to_string(),
            );
        }
        let (token, token_file) = match (token, token_file) {
            (_, Some(file)) => (None, Some(file)),
            (Some(key), None) => {
                notes.push(
                    "the API token was copied into server.token; moving it to a file \
                     referenced by server.token_file keeps it out of your config"
                        .to_string(),
                );
                (Some(key), None)
            }
            (None, None) => (None, None),
        };

        let mut layouts: Vec<ColumnLayout> = self
            .column_layouts
            .unwrap_or_default()
            .into_iter()
            .map(|layout| ColumnLayout {
                name: layout.name,
                description: layout.description,
                columns: convert_columns(layout.columns, &mut notes),
            })
            .collect();

        // cria supports a bare `table_columns` list as well as named layouts. It becomes
        // a layout named "imported" so the two concepts stay one concept here.
        if let Some(columns) = self.table_columns {
            let converted = convert_columns(columns, &mut notes);
            if !converted.is_empty() {
                layouts.push(ColumnLayout {
                    name: "imported".to_string(),
                    description: Some("Carried over from cria's table_columns".to_string()),
                    columns: converted,
                });
            }
        }

        let quick_actions = self
            .quick_actions
            .unwrap_or_default()
            .into_iter()
            .filter_map(|action| convert_quick_action(action, &mut notes))
            .collect();

        Migration {
            config: Config {
                server: ServerConfig {
                    url,
                    token,
                    token_file,
                    username: None,
                },
                sync: SyncConfig {
                    interval_seconds: self.refresh_interval_seconds.unwrap_or(300),
                    enabled: self.auto_refresh.unwrap_or(true),
                },
                view: ViewConfig {
                    default_project: self.default_project,
                    default_filter: self.default_filter,
                    active_layout: self.active_layout,
                    layouts,
                },
                quick_actions,
            },
            notes,
        }
    }
}

/// Translate cria's columns, dropping the disabled ones and any criax has no field for.
fn convert_columns(columns: Vec<CriaTableColumn>, notes: &mut Vec<String>) -> Vec<ColumnSpec> {
    columns
        .into_iter()
        // cria carries disabled columns in the list; criax expresses that by leaving
        // them out, so there is one representation of "not shown" rather than two.
        .filter(|c| c.enabled)
        .filter_map(|c| match convert_column_type(&c.column_type) {
            Some(column) => Some(ColumnSpec {
                column,
                heading: c.name.filter(|n| n != column.default_heading()),
                width_percent: c.width_percentage,
                min_width: c.min_width,
                max_width: c.max_width,
                wrap: c.wrap_text.unwrap_or(false),
                sort: c.sort.map(|s| ColumnSort {
                    order: s.order,
                    direction: match s.direction.as_deref() {
                        Some("desc" | "Desc") => SortDirection::Desc,
                        _ => SortDirection::Asc,
                    },
                }),
            }),
            None => {
                notes.push(format!(
                    "dropped column {:?}, which criax does not have",
                    c.column_type
                ));
                None
            }
        })
        .collect()
}

/// Map cria's column-type spelling onto criax's.
fn convert_column_type(raw: &str) -> Option<Column> {
    // cria serialises these in several cases across its own versions, so normalise
    // rather than matching one spelling and quietly dropping the rest.
    let normalised: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match normalised.as_str() {
        "title" => Some(Column::Title),
        "project" => Some(Column::Project),
        "labels" => Some(Column::Labels),
        "duedate" => Some(Column::DueDate),
        "startdate" => Some(Column::StartDate),
        "priority" => Some(Column::Priority),
        "status" | "done" => Some(Column::Status),
        "assignees" => Some(Column::Assignees),
        "created" => Some(Column::Created),
        "updated" => Some(Column::Updated),
        _ => None,
    }
}

/// Translate one quick action, reporting the ones that cannot be.
fn convert_quick_action(action: CriaQuickAction, notes: &mut Vec<String>) -> Option<QuickAction> {
    let Some(key) = action.key.chars().next() else {
        notes.push("dropped a quick action with an empty key".to_string());
        return None;
    };
    if action.key.chars().count() > 1 {
        notes.push(format!(
            "quick action {:?} had a multi-character key; using {key:?}",
            action.key
        ));
    }

    let kind = match action.action.to_ascii_lowercase().as_str() {
        "project" => QuickActionKind::Project(action.target),
        "label" => QuickActionKind::Label(action.target),
        "priority" => match action.target.trim().parse::<u8>() {
            Ok(priority @ 1..=5) => QuickActionKind::Priority(priority),
            _ => {
                notes.push(format!(
                    "dropped quick action {key:?}: priority {:?} is not 1-5",
                    action.target
                ));
                return None;
            }
        },
        other => {
            notes.push(format!(
                "dropped quick action {key:?}: criax has no {other:?} action"
            ));
            return None;
        }
    };

    Some(QuickAction { key, kind })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A cria config in the shape its README documents.
    const CRIA_YAML: &str = "\
api_url: https://vikunja.example.com/api/v1
api_key_file: ~/.config/cria/token
default_project: Work
default_filter: done = false
refresh_interval_seconds: 120
auto_refresh: true
active_layout: wide
quick_actions:
  - key: u
    action: priority
    target: '5'
  - key: w
    action: project
    target: Work
  - key: b
    action: label
    target: blocked
column_layouts:
  - name: wide
    description: Everything
    columns:
      - name: Title
        column_type: Title
        enabled: true
        min_width: 20
        wrap_text: true
      - name: Deadline
        column_type: DueDate
        enabled: true
        max_width: 12
        sort:
          order: 1
          direction: desc
      - name: Hidden
        column_type: Created
        enabled: false
";

    #[test]
    fn a_realistic_cria_config_imports_cleanly() {
        let migration = from_cria_yaml(CRIA_YAML).unwrap();
        let config = &migration.config;

        // The /api/v1 suffix comes off: criax stores the server, not the API root.
        assert_eq!(config.server.url, "https://vikunja.example.com");
        assert_eq!(
            config.server.token_file,
            Some(PathBuf::from("~/.config/cria/token"))
        );
        assert_eq!(config.sync.interval_seconds, 120);
        assert_eq!(config.view.default_project.as_deref(), Some("Work"));
        assert_eq!(config.view.active_layout.as_deref(), Some("wide"));
        assert_eq!(config.quick_actions.len(), 3);
        assert_eq!(config.quick_actions[0].kind, QuickActionKind::Priority(5));
        assert_eq!(
            config.quick_actions[2].kind,
            QuickActionKind::Label("blocked".to_string())
        );
    }

    #[test]
    fn disabled_columns_are_dropped_rather_than_carried_as_disabled() {
        let migration = from_cria_yaml(CRIA_YAML).unwrap();
        let layout = &migration.config.view.layouts[0];
        assert_eq!(
            layout.columns.len(),
            2,
            "the disabled column should be gone"
        );
        assert_eq!(layout.columns[0].column, Column::Title);
        assert!(layout.columns[0].wrap);
        assert_eq!(layout.columns[1].heading(), "Deadline");
        assert_eq!(
            layout.sort_keys(),
            vec![(Column::DueDate, SortDirection::Desc)]
        );
    }

    #[test]
    fn a_heading_matching_the_default_is_not_carried_as_an_override() {
        let migration = from_cria_yaml(CRIA_YAML).unwrap();
        let title = &migration.config.view.layouts[0].columns[0];
        assert_eq!(title.heading, None, "\"Title\" is already the default");
    }

    #[test]
    fn the_imported_config_is_one_criax_will_actually_load() {
        // The real test of a migration: its output has to survive the strict loader,
        // including `deny_unknown_fields`.
        let migration = from_cria_yaml(CRIA_YAML).unwrap();
        let yaml = migration.config.to_yaml().unwrap();
        let reloaded = Config::from_yaml(&yaml).unwrap();
        assert_eq!(reloaded, migration.config);
    }

    #[test]
    fn unknown_keys_do_not_stop_the_import() {
        // cria's config may carry keys from a version criax never saw. Refusing the whole
        // import over one of them would help nobody.
        let yaml = "api_url: https://x/api/v1\nsome_future_cria_setting: 42\n";
        let migration = from_cria_yaml(yaml).unwrap();
        assert_eq!(migration.config.server.url, "https://x");
    }

    #[test]
    fn what_cannot_be_carried_across_is_reported() {
        let yaml = "\
api_url: https://x
quick_actions:
  - key: z
    action: teleport
    target: away
  - key: p
    action: priority
    target: '9'
column_layouts:
  - name: odd
    columns:
      - column_type: Gantt
        enabled: true
";
        let migration = from_cria_yaml(yaml).unwrap();
        assert!(migration.config.quick_actions.is_empty());
        assert_eq!(migration.notes.len(), 3, "notes: {:#?}", migration.notes);
        assert!(migration.notes.iter().any(|n| n.contains("teleport")));
        assert!(migration.notes.iter().any(|n| n.contains("not 1-5")));
        assert!(migration.notes.iter().any(|n| n.contains("Gantt")));
    }

    #[test]
    fn an_inline_api_key_is_carried_but_flagged() {
        let yaml = "api_url: https://x\napi_key: tk_inline\n";
        let migration = from_cria_yaml(yaml).unwrap();
        assert_eq!(migration.config.server.token.as_deref(), Some("tk_inline"));
        assert!(migration.notes.iter().any(|n| n.contains("token_file")));
    }

    #[test]
    fn both_credential_forms_resolve_to_the_file() {
        // criax refuses both at once, so the import has to pick -- and the file is the
        // better half to keep.
        let yaml = "api_url: https://x\napi_key: tk_inline\napi_key_file: ~/.token\n";
        let migration = from_cria_yaml(yaml).unwrap();
        assert_eq!(migration.config.server.token, None);
        assert_eq!(
            migration.config.server.token_file,
            Some(PathBuf::from("~/.token"))
        );
        migration
            .config
            .api_token(Path::new("/tmp/config.yaml"))
            .expect_err("the file does not exist, but the config itself is unambiguous");
    }

    #[test]
    fn a_bare_table_columns_list_becomes_a_layout() {
        let yaml = "\
api_url: https://x
table_columns:
  - column_type: Title
    enabled: true
  - column_type: Priority
    enabled: true
";
        let migration = from_cria_yaml(yaml).unwrap();
        let layouts = &migration.config.view.layouts;
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "imported");
        assert_eq!(layouts[0].columns.len(), 2);
    }

    #[test]
    fn a_missing_api_url_is_reported_not_invented() {
        let migration = from_cria_yaml("default_project: Work\n").unwrap();
        assert!(migration.config.server.url.is_empty());
        assert!(migration.notes.iter().any(|n| n.contains("server.url")));
    }
}
