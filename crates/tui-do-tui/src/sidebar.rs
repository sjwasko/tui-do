//! The project tree, flattened for display.
//!
//! Pure: given the projects the store returned and which nodes are collapsed, produce the
//! rows to draw. Selection is a [`SidebarTarget`] rather than a row index, so a project
//! arriving or leaving does not move the cursor to something else.

use std::collections::{HashMap, HashSet};

use tui_do_core::models::{Project, ProjectId};
use tui_do_core::store::ProjectCounts;

/// Something the sidebar can select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SidebarTarget {
    /// Every task in the store.
    #[default]
    AllTasks,
    /// Favourited tasks.
    Favorites,
    /// One project.
    Project(ProjectId),
}

/// One drawn line of the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarRow {
    /// A section heading. Not selectable.
    Heading(&'static str),
    /// A place tasks can come from.
    Target {
        /// What selecting this row shows.
        target: SidebarTarget,
        /// The text to draw.
        title: String,
        /// Open tasks, drawn on the right.
        open: i64,
        /// How deep in the tree, for indentation.
        depth: u16,
        /// Whether it has children at all.
        has_children: bool,
        /// Whether those children are hidden.
        collapsed: bool,
    },
}

impl SidebarRow {
    /// The target this row selects, if it is selectable.
    #[must_use]
    pub const fn target(&self) -> Option<SidebarTarget> {
        match self {
            Self::Heading(_) => None,
            Self::Target { target, .. } => Some(*target),
        }
    }
}

/// The sidebar's own state, which survives reloads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SidebarState {
    /// What is selected.
    pub selected: SidebarTarget,
    /// Projects whose children are hidden. Collapsed rather than expanded, so a first
    /// launch shows the tree rather than a row of closed folders.
    pub collapsed: HashSet<ProjectId>,
    /// First visible row, for scrolling.
    pub offset: usize,
}

/// Build the rows to draw.
///
/// Pseudo-projects are excluded: Favourites has its own row, and the others are saved
/// filters that Phase 6 will present properly. Archived projects are hidden, matching the
/// web UI, which is also why a project count here can differ from the server's.
#[must_use]
pub fn rows(projects: &[Project], counts: &ProjectCounts, state: &SidebarState) -> Vec<SidebarRow> {
    let mut rows = vec![SidebarRow::Target {
        target: SidebarTarget::AllTasks,
        title: "All tasks".to_string(),
        open: counts.by_project.values().map(|count| count.open).sum(),
        depth: 0,
        has_children: false,
        collapsed: false,
    }];

    if counts.favorites.total() > 0 {
        rows.push(SidebarRow::Target {
            target: SidebarTarget::Favorites,
            title: "Favorites".to_string(),
            open: counts.favorites.open,
            depth: 0,
            has_children: false,
            collapsed: false,
        });
    }

    let real: Vec<&Project> = projects
        .iter()
        .filter(|project| project.id.get() > 0 && !project.is_archived)
        .collect();
    if real.is_empty() {
        return rows;
    }
    rows.push(SidebarRow::Heading("Projects"));

    let present: HashSet<ProjectId> = real.iter().map(|project| project.id).collect();
    let mut children: HashMap<ProjectId, Vec<&Project>> = HashMap::new();
    for project in &real {
        // A project whose parent was filtered out — archived, or simply not returned —
        // is drawn at the top level rather than vanishing with it.
        let parent = if present.contains(&project.parent_project_id) {
            project.parent_project_id
        } else {
            ProjectId(0)
        };
        children.entry(parent).or_default().push(project);
    }
    for group in children.values_mut() {
        group.sort_by(|a, b| {
            a.position
                .partial_cmp(&b.position)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        });
    }

    // An explicit stack rather than recursion, and a `seen` set: a parent cycle is a
    // server bug we should render oddly, not hang on.
    let mut seen: HashSet<ProjectId> = HashSet::new();
    let mut stack: Vec<(&Project, u16)> = children
        .get(&ProjectId(0))
        .map(|roots| roots.iter().rev().map(|p| (*p, 0)).collect())
        .unwrap_or_default();

    while let Some((project, depth)) = stack.pop() {
        if !seen.insert(project.id) {
            continue;
        }
        let kids = children.get(&project.id);
        let collapsed = state.collapsed.contains(&project.id);
        rows.push(SidebarRow::Target {
            target: SidebarTarget::Project(project.id),
            title: project.title.clone(),
            open: counts.for_project(project.id).open,
            depth,
            has_children: kids.is_some_and(|kids| !kids.is_empty()),
            collapsed,
        });
        if collapsed {
            continue;
        }
        if let Some(kids) = kids {
            stack.extend(kids.iter().rev().map(|kid| (*kid, depth + 1)));
        }
    }

    rows
}

/// The selectable targets, in display order.
#[must_use]
pub fn targets(rows: &[SidebarRow]) -> Vec<SidebarTarget> {
    rows.iter().filter_map(SidebarRow::target).collect()
}

/// The parent of a project, when the tree holds one.
#[must_use]
pub fn parent_of(projects: &[Project], id: ProjectId) -> Option<ProjectId> {
    let parent = projects
        .iter()
        .find(|project| project.id == id)?
        .parent_project_id;
    projects
        .iter()
        .any(|project| project.id == parent)
        .then_some(parent)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tui_do_core::store::TaskCount;

    fn project(id: i64, title: &str, parent: i64) -> Project {
        Project {
            id: ProjectId(id),
            title: title.to_string(),
            parent_project_id: ProjectId(parent),
            ..Project::default()
        }
    }

    fn counts() -> ProjectCounts {
        ProjectCounts {
            by_project: HashMap::from([(ProjectId(1), TaskCount { open: 3, done: 1 })]),
            favorites: TaskCount::default(),
        }
    }

    fn titles(rows: &[SidebarRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                SidebarRow::Heading(text) => (*text).to_string(),
                SidebarRow::Target { title, depth, .. } => {
                    format!("{}{title}", "  ".repeat(*depth as usize))
                }
            })
            .collect()
    }

    #[test]
    fn children_nest_under_their_parent_in_position_order() {
        let projects = vec![
            project(1, "Work", 0),
            project(3, "Later", 1),
            project(2, "Errands", 1),
            project(4, "Personal", 0),
        ];
        let rows = rows(&projects, &counts(), &SidebarState::default());
        assert_eq!(
            titles(&rows),
            [
                "All tasks",
                "Projects",
                "Personal",
                "Work",
                "  Errands",
                "  Later",
            ]
        );
    }

    #[test]
    fn a_collapsed_project_hides_its_children_but_stays_selectable() {
        let projects = vec![project(1, "Work", 0), project(2, "Errands", 1)];
        let state = SidebarState {
            collapsed: HashSet::from([ProjectId(1)]),
            ..SidebarState::default()
        };
        let rows = rows(&projects, &counts(), &state);
        assert!(!titles(&rows).contains(&"  Errands".to_string()));
        assert!(targets(&rows).contains(&SidebarTarget::Project(ProjectId(1))));
    }

    #[test]
    fn pseudo_projects_and_archived_ones_stay_out() {
        let mut archived = project(5, "Old", 0);
        archived.is_archived = true;
        let projects = vec![project(-1, "Favorites", 0), archived, project(1, "Work", 0)];
        let rows = rows(&projects, &counts(), &SidebarState::default());
        let titles = titles(&rows);
        assert!(!titles.contains(&"Favorites".to_string()));
        assert!(!titles.contains(&"Old".to_string()));
        assert!(titles.contains(&"Work".to_string()));
    }

    #[test]
    fn favorites_appears_only_when_there_are_some() {
        let projects = vec![project(1, "Work", 0)];
        let without = rows(&projects, &counts(), &SidebarState::default());
        assert!(!targets(&without).contains(&SidebarTarget::Favorites));

        let with_favorites = ProjectCounts {
            favorites: TaskCount { open: 2, done: 0 },
            ..counts()
        };
        let with = rows(&projects, &with_favorites, &SidebarState::default());
        assert!(targets(&with).contains(&SidebarTarget::Favorites));
    }

    #[test]
    fn an_orphan_is_drawn_at_the_top_rather_than_disappearing() {
        // Its parent is archived, so it was filtered out of the tree.
        let mut parent = project(9, "Archive", 0);
        parent.is_archived = true;
        let projects = vec![parent, project(2, "Stranded", 9)];
        let rows = rows(&projects, &counts(), &SidebarState::default());
        assert!(titles(&rows).contains(&"Stranded".to_string()));
    }

    #[test]
    fn a_parent_cycle_renders_rather_than_hanging() {
        let projects = vec![project(1, "A", 2), project(2, "B", 1)];
        let rows = rows(&projects, &counts(), &SidebarState::default());
        // Neither is a root, so the tree has no entry point and only the fixed rows show.
        // The point of the test is that it terminates.
        assert!(!titles(&rows).is_empty());
    }
}
