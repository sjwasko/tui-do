//! What the task list is asking the store for.
//!
//! A [`Query`] is a description, never a result: `update` builds one, the effect runtime
//! answers it, and the answer arrives as a message. The [`QueryId`] riding along is what
//! makes that safe — see [`QueryId`] for why.

use criax_core::config::columns::{Column, ColumnLayout, SortDirection};
use criax_core::models::{LabelId, ProjectId};
use criax_core::store::{TaskFilter, TaskOrder, TaskSort};

/// The most rows one query will return.
///
/// Not a page size: the whole result set lives in the model so scrolling costs nothing,
/// and 1,900 open tasks is a couple of megabytes. This exists so a pathological filter
/// cannot eat the process, and the status line says when it bites.
pub const TASK_LIMIT: u32 = 10_000;

/// Identifies one request for tasks.
///
/// Every [`crate::Effect::LoadTasks`] carries one, and an answer whose id is no longer
/// the model's current id is dropped. Holding `j` down the sidebar issues a query per
/// project, and without this the slowest one wins whichever order they finish in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct QueryId(u64);

impl QueryId {
    /// The next id in sequence.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Which tasks the list is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// Every task the store holds.
    #[default]
    All,
    /// One project.
    Project(ProjectId),
    /// Favourites, which cut across projects.
    Favorites,
    /// Everything carrying one label.
    Label(LabelId),
}

/// A description of the task list to show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    /// Where the tasks come from.
    pub scope: Scope,
    /// A case-insensitive substring of the title, when the user has searched.
    pub search: Option<String>,
    /// Whether completed tasks appear alongside open ones.
    pub include_done: bool,
    /// How the rows are ordered.
    pub sort: TaskSort,
}

impl Query {
    /// The store filter this query means.
    #[must_use]
    pub fn filter(&self) -> TaskFilter {
        TaskFilter {
            project: match self.scope {
                Scope::Project(id) => Some(id),
                _ => None,
            },
            label: match self.scope {
                Scope::Label(id) => Some(id),
                _ => None,
            },
            favorite: match self.scope {
                Scope::Favorites => Some(true),
                _ => None,
            },
            // `Some(false)` rather than `None`: an unfiltered listing includes done
            // tasks, which is exactly the surprise that makes a list look wrong.
            done: if self.include_done { None } else { Some(false) },
            search: self.search.clone(),
            limit: Some(TASK_LIMIT),
        }
    }
}

/// The store ordering a column means, when it has one.
///
/// Partial on purpose, and the line is principled rather than arbitrary: a column backed
/// by a column of `tasks` can be ordered in SQL, and one backed by a join — labels,
/// assignees, the project's title — cannot. Sorting in the model instead would duplicate
/// the store's careful "dateless tasks sort last, because SQLite puts `NULL` first" rule
/// and would eventually disagree with it.
#[must_use]
pub const fn order_for(column: Column) -> Option<TaskOrder> {
    match column {
        Column::Title => Some(TaskOrder::Title),
        Column::DueDate => Some(TaskOrder::DueDate),
        Column::StartDate => Some(TaskOrder::StartDate),
        Column::Priority => Some(TaskOrder::Priority),
        Column::Created => Some(TaskOrder::Created),
        Column::Updated => Some(TaskOrder::Updated),
        Column::Status => Some(TaskOrder::Status),
        Column::PercentDone => Some(TaskOrder::PercentDone),
        Column::Identifier => Some(TaskOrder::Id),
        Column::Project | Column::Labels | Column::Assignees => None,
    }
}

/// The ordering a layout asks for.
///
/// The layout is the single source of order — that is why `sort_keys` lives on it. A
/// layout whose sort column has no store ordering falls back to the default rather than
/// failing: the user loses their preference, not their task list.
#[must_use]
pub fn sort_for(layout: &ColumnLayout) -> TaskSort {
    layout
        .sort_keys()
        .into_iter()
        .find_map(|(column, direction)| {
            order_for(column).map(|order| TaskSort {
                order,
                descending: direction == SortDirection::Desc,
            })
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use criax_core::config::columns::{ColumnSort, ColumnSpec};

    #[test]
    fn a_scope_picks_exactly_one_store_predicate() {
        let by_project = Query {
            scope: Scope::Project(ProjectId(4)),
            ..Query::default()
        }
        .filter();
        assert_eq!(by_project.project, Some(ProjectId(4)));
        assert_eq!(by_project.label, None);
        assert_eq!(by_project.favorite, None);

        let favorites = Query {
            scope: Scope::Favorites,
            ..Query::default()
        }
        .filter();
        assert_eq!(favorites.favorite, Some(true));
        assert_eq!(favorites.project, None);
    }

    #[test]
    fn done_tasks_are_excluded_unless_asked_for() {
        assert_eq!(Query::default().filter().done, Some(false));
        assert_eq!(
            Query {
                include_done: true,
                ..Query::default()
            }
            .filter()
            .done,
            None
        );
    }

    #[test]
    fn the_default_layout_sorts_by_due_date_because_it_says_so() {
        let layouts = ColumnLayout::defaults();
        let default = layouts.first().unwrap();
        assert_eq!(
            sort_for(default),
            TaskSort {
                order: TaskOrder::DueDate,
                descending: false
            }
        );
    }

    #[test]
    fn a_layout_sorted_by_an_unorderable_column_falls_back_rather_than_failing() {
        let layout = ColumnLayout {
            name: "by label".into(),
            description: None,
            columns: vec![ColumnSpec {
                sort: Some(ColumnSort {
                    order: 1,
                    direction: SortDirection::Asc,
                }),
                ..ColumnSpec::new(Column::Labels)
            }],
        };
        assert_eq!(sort_for(&layout), TaskSort::default());
    }

    #[test]
    fn a_later_sortable_column_is_used_when_the_first_one_cannot_sort() {
        let layout = ColumnLayout {
            name: "mixed".into(),
            description: None,
            columns: vec![
                ColumnSpec {
                    sort: Some(ColumnSort {
                        order: 1,
                        direction: SortDirection::Asc,
                    }),
                    ..ColumnSpec::new(Column::Assignees)
                },
                ColumnSpec {
                    sort: Some(ColumnSort {
                        order: 2,
                        direction: SortDirection::Desc,
                    }),
                    ..ColumnSpec::new(Column::Priority)
                },
            ],
        };
        assert_eq!(
            sort_for(&layout),
            TaskSort {
                order: TaskOrder::Priority,
                descending: true
            }
        );
    }

    #[test]
    fn query_ids_do_not_repeat_in_a_session() {
        let mut id = QueryId::default();
        let mut seen = vec![id];
        for _ in 0..10 {
            id = id.next();
            assert!(!seen.contains(&id));
            seen.push(id);
        }
    }
}
