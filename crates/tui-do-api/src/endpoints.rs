//! Every URL path template tui-do is allowed to call.
//!
//! These constants are the single source of truth for the client's URL construction,
//! and `tests/conformance.rs` asserts that each one exists in `spec/vikunja.json`. A
//! path that is not declared here cannot be built, and a path declared here that the
//! server does not serve fails the build.
//!
//! This exists because cria hardcoded `/tasks/all` inline at three call sites. Upstream
//! renamed it to `/tasks`, and the breakage surfaced as runtime 404s answered with a
//! 195-line "Method 1 / Method 2 / Method 3" chain of fallback guesses. Refreshing the
//! spec with `cargo xtask fetch-spec` now turns that same rename into a failing test.
//!
//! Templates use the spec's own parameter spelling — `{id}`, `{taskID}`, `{project}` —
//! because the conformance check compares them literally against the spec's path keys.
//! Vikunja is not consistent about which spelling it uses, so copy, do not normalise.

/// Server metadata: version, `max_items_per_page`, enabled features.
///
/// Read at startup; `max_items_per_page` is where the pagination cap comes from rather
/// than a hardcoded guess.
pub const INFO: &str = "/info";

/// Exchange username and password for a JWT.
pub const LOGIN: &str = "/login";

/// Renew a JWT before it expires.
pub const TOKEN_REFRESH: &str = "/user/token/refresh";

/// Invalidate the current session.
pub const LOGOUT: &str = "/user/logout";

/// The authenticated user.
pub const CURRENT_USER: &str = "/user";

/// All tasks across all projects, paginated and filterable.
///
/// The endpoint cria called `/tasks/all`. That spelling no longer exists.
pub const TASKS: &str = "/tasks";

/// A single task: `GET` to read, `POST` to update, `DELETE` to remove.
pub const TASK: &str = "/tasks/{id}";

/// Tasks belonging to one project view. How the web frontend loads a project.
pub const VIEW_TASKS: &str = "/projects/{id}/views/{view}/tasks";

/// Create a task in a project.
pub const PROJECT_TASKS: &str = "/projects/{id}/tasks";

/// All projects the user can see.
pub const PROJECTS: &str = "/projects";

/// A single project.
pub const PROJECT: &str = "/projects/{id}";

/// The views defined on a project.
pub const PROJECT_VIEWS: &str = "/projects/{project}/views";

/// Buckets in a Kanban view.
pub const VIEW_BUCKETS: &str = "/projects/{id}/views/{view}/buckets";

/// All labels.
pub const LABELS: &str = "/labels";

/// A single label.
pub const LABEL: &str = "/labels/{id}";

/// Labels on a task: `GET` to list, `PUT` to attach.
pub const TASK_LABELS: &str = "/tasks/{task}/labels";

/// Detach a label from a task.
pub const TASK_LABEL: &str = "/tasks/{task}/labels/{label}";

/// Assignees on a task.
pub const TASK_ASSIGNEES: &str = "/tasks/{taskID}/assignees";

/// Remove one assignee from a task.
pub const TASK_ASSIGNEE: &str = "/tasks/{taskID}/assignees/{userID}";

/// Comments on a task.
pub const TASK_COMMENTS: &str = "/tasks/{taskID}/comments";

/// A single comment.
pub const TASK_COMMENT: &str = "/tasks/{taskID}/comments/{commentID}";

/// Attachments on a task.
pub const TASK_ATTACHMENTS: &str = "/tasks/{id}/attachments";

/// A single attachment.
pub const TASK_ATTACHMENT: &str = "/tasks/{id}/attachments/{attachmentID}";

/// Create a relation between two tasks.
pub const TASK_RELATIONS: &str = "/tasks/{taskID}/relations";

/// Remove a relation.
pub const TASK_RELATION: &str = "/tasks/{taskID}/relations/{relationKind}/{otherTaskID}";

/// Saved filters.
pub const FILTERS: &str = "/filters";

/// A single saved filter.
pub const FILTER: &str = "/filters/{id}";

/// Every template above, for the conformance test to walk.
///
/// Adding a constant without adding it here would let it escape the check, so the test
/// also asserts this list is exhaustive by count.
pub const ALL: &[&str] = &[
    INFO,
    LOGIN,
    TOKEN_REFRESH,
    LOGOUT,
    CURRENT_USER,
    TASKS,
    TASK,
    VIEW_TASKS,
    PROJECT_TASKS,
    PROJECTS,
    PROJECT,
    PROJECT_VIEWS,
    VIEW_BUCKETS,
    LABELS,
    LABEL,
    TASK_LABELS,
    TASK_LABEL,
    TASK_ASSIGNEES,
    TASK_ASSIGNEE,
    TASK_COMMENTS,
    TASK_COMMENT,
    TASK_ATTACHMENTS,
    TASK_ATTACHMENT,
    TASK_RELATIONS,
    TASK_RELATION,
    FILTERS,
    FILTER,
];
