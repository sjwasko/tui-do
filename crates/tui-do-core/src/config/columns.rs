//! Task-list column layouts.
//!
//! Ported from cria's schema, which is documented in its `COLUMN_LAYOUTS.md` and is the
//! part of that config worth keeping: named layouts the user switches between, each a
//! list of columns with independent width, wrapping and sort rules.

use serde::{Deserialize, Serialize};

/// A field of a task that can be shown as a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    /// The task title.
    Title,
    /// The project the task belongs to.
    Project,
    /// Attached labels.
    Labels,
    /// Due date.
    DueDate,
    /// Start date.
    StartDate,
    /// Priority, 1 to 5.
    Priority,
    /// Done or not.
    Status,
    /// Assigned users.
    Assignees,
    /// Creation timestamp.
    Created,
    /// Last-modified timestamp.
    Updated,
    /// The `WORK-42` style identifier.
    Identifier,
    /// Percent complete.
    PercentDone,
}

impl Column {
    /// The heading shown when the layout does not override it.
    #[must_use]
    pub fn default_heading(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Project => "Project",
            Self::Labels => "Labels",
            Self::DueDate => "Due",
            Self::StartDate => "Start",
            Self::Priority => "Pri",
            Self::Status => "Done",
            Self::Assignees => "Assignees",
            Self::Created => "Created",
            Self::Updated => "Updated",
            Self::Identifier => "ID",
            Self::PercentDone => "%",
        }
    }
}

/// Which way a column sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    /// Smallest first.
    #[default]
    Asc,
    /// Largest first.
    Desc,
}

/// A column's contribution to the list's sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSort {
    /// Sort priority: 1 is the primary key, 2 the tie-breaker, and so on.
    pub order: u16,

    /// Direction for this key.
    #[serde(default)]
    pub direction: SortDirection,
}

/// One column in a layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnSpec {
    /// Which task field this shows.
    pub column: Column,

    /// Heading override. Falls back to [`Column::default_heading`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,

    /// Share of the table width, as a percentage. `None` means "share what is left".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_percent: Option<u16>,

    /// Never render narrower than this many characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_width: Option<u16>,

    /// Never render wider than this many characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u16>,

    /// Wrap the cell's text instead of truncating it.
    #[serde(default)]
    pub wrap: bool,

    /// Include this column in the list's sort order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<ColumnSort>,
}

impl ColumnSpec {
    /// A column with every option left at its default.
    #[must_use]
    pub fn new(column: Column) -> Self {
        Self {
            column,
            heading: None,
            width_percent: None,
            min_width: None,
            max_width: None,
            wrap: false,
            sort: None,
        }
    }

    /// The heading to render.
    #[must_use]
    pub fn heading(&self) -> &str {
        self.heading
            .as_deref()
            .unwrap_or_else(|| self.column.default_heading())
    }
}

/// A named set of columns the user can switch to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnLayout {
    /// Name used to select the layout, and shown in the switcher.
    pub name: String,

    /// Optional one-line explanation, shown alongside the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// The columns, in display order.
    pub columns: Vec<ColumnSpec>,
}

impl ColumnLayout {
    /// The layouts shipped when the user has configured none.
    ///
    /// None of them wrap. A wrapped title costs two or three rows to say what a truncated
    /// one says in one, so a short window shows a third of the tasks it could — and the
    /// list is the thing the user is scanning. What the truncation hides is a keystroke
    /// away in the preview, which exists for exactly that.
    ///
    /// `wrap` stays in the schema, and the renderer and the scroll maths both still
    /// handle it, because it is the user's column layout to configure.
    #[must_use]
    pub fn defaults() -> Vec<Self> {
        vec![
            Self {
                name: "default".to_string(),
                description: Some("Title, project, labels and due date".to_string()),
                columns: vec![
                    ColumnSpec {
                        min_width: Some(20),
                        ..ColumnSpec::new(Column::Title)
                    },
                    ColumnSpec {
                        min_width: Some(10),
                        max_width: Some(20),
                        ..ColumnSpec::new(Column::Project)
                    },
                    ColumnSpec {
                        max_width: Some(24),
                        ..ColumnSpec::new(Column::Labels)
                    },
                    ColumnSpec {
                        max_width: Some(12),
                        sort: Some(ColumnSort {
                            order: 1,
                            direction: SortDirection::Asc,
                        }),
                        ..ColumnSpec::new(Column::DueDate)
                    },
                ],
            },
            Self {
                name: "compact".to_string(),
                description: Some("Just the title and when it is due".to_string()),
                columns: vec![
                    ColumnSpec {
                        min_width: Some(24),
                        ..ColumnSpec::new(Column::Title)
                    },
                    ColumnSpec {
                        max_width: Some(12),
                        ..ColumnSpec::new(Column::DueDate)
                    },
                ],
            },
            Self {
                name: "detailed".to_string(),
                description: Some("Everything worth seeing at a glance".to_string()),
                columns: vec![
                    ColumnSpec {
                        max_width: Some(10),
                        ..ColumnSpec::new(Column::Identifier)
                    },
                    ColumnSpec {
                        min_width: Some(20),
                        ..ColumnSpec::new(Column::Title)
                    },
                    ColumnSpec::new(Column::Project),
                    ColumnSpec::new(Column::Labels),
                    ColumnSpec {
                        max_width: Some(5),
                        ..ColumnSpec::new(Column::Priority)
                    },
                    ColumnSpec {
                        max_width: Some(12),
                        sort: Some(ColumnSort {
                            order: 1,
                            direction: SortDirection::Asc,
                        }),
                        ..ColumnSpec::new(Column::DueDate)
                    },
                    ColumnSpec::new(Column::Assignees),
                ],
            },
        ]
    }

    /// The sort keys, in priority order.
    ///
    /// Reading them off the layout rather than storing a separate sort setting is what
    /// keeps "what is shown" and "how it is ordered" from drifting apart.
    #[must_use]
    pub fn sort_keys(&self) -> Vec<(Column, SortDirection)> {
        let mut keyed: Vec<(u16, Column, SortDirection)> = self
            .columns
            .iter()
            .filter_map(|c| c.sort.map(|s| (s.order, c.column, s.direction)))
            .collect();
        keyed.sort_by_key(|(order, _, _)| *order);
        keyed
            .into_iter()
            .map(|(_, column, direction)| (column, direction))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_default_layout_has_a_title_column() {
        for layout in ColumnLayout::defaults() {
            assert!(
                layout.columns.iter().any(|c| c.column == Column::Title),
                "layout {} has no title column",
                layout.name
            );
        }
    }

    #[test]
    fn default_layout_names_are_unique() {
        let layouts = ColumnLayout::defaults();
        let mut names: Vec<&str> = layouts.iter().map(|l| l.name.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn sort_keys_come_back_in_priority_order() {
        let layout = ColumnLayout {
            name: "t".into(),
            description: None,
            columns: vec![
                ColumnSpec {
                    sort: Some(ColumnSort {
                        order: 2,
                        direction: SortDirection::Desc,
                    }),
                    ..ColumnSpec::new(Column::Priority)
                },
                ColumnSpec::new(Column::Title),
                ColumnSpec {
                    sort: Some(ColumnSort {
                        order: 1,
                        direction: SortDirection::Asc,
                    }),
                    ..ColumnSpec::new(Column::DueDate)
                },
            ],
        };
        assert_eq!(
            layout.sort_keys(),
            vec![
                (Column::DueDate, SortDirection::Asc),
                (Column::Priority, SortDirection::Desc),
            ]
        );
    }

    #[test]
    fn a_column_falls_back_to_its_default_heading() {
        assert_eq!(ColumnSpec::new(Column::DueDate).heading(), "Due");
        let renamed = ColumnSpec {
            heading: Some("Deadline".into()),
            ..ColumnSpec::new(Column::DueDate)
        };
        assert_eq!(renamed.heading(), "Deadline");
    }

    #[test]
    fn a_misspelled_column_field_is_an_error_not_a_silent_default() {
        // cria ignores unknown keys, so a typo in a layout does nothing and says nothing.
        let yaml = "column: title\nwidht_percent: 40\n";
        assert!(serde_yaml_ng::from_str::<ColumnSpec>(yaml).is_err());
    }

    #[test]
    fn layouts_round_trip_through_yaml() {
        for layout in ColumnLayout::defaults() {
            let yaml = serde_yaml_ng::to_string(&layout).unwrap();
            let parsed: ColumnLayout = serde_yaml_ng::from_str(&yaml).unwrap();
            assert_eq!(parsed, layout);
        }
    }
}
