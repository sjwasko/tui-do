//! Query parameters for the collection endpoints.
//!
//! Built as a type rather than assembled at each call site so that a filter, a sort and
//! a search cannot be spelled three different ways in three places — which is how cria
//! ended up with `/tasks/all` hardcoded at three call sites and only one of them fixed.

use std::fmt;

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Order {
    /// Ascending. Vikunja's default.
    #[default]
    Asc,
    /// Descending.
    Desc,
}

impl fmt::Display for Order {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        })
    }
}

/// Extra data to load alongside each task.
///
/// Note the cost of [`Expand::Subtasks`]: the spec states it "may result in more tasks
/// than the pagination limit being returned", so a page fetched with it can exceed
/// `per_page` and the same subtask can arrive twice across pages. Deduplicate by id
/// before storing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expand {
    /// Fetch parent tasks first, then their subtasks in a second pass.
    Subtasks,
    /// Include each task's Kanban bucket.
    Buckets,
    /// Include reactions.
    Reactions,
    /// Include the first 50 comments of each task.
    Comments,
}

impl fmt::Display for Expand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Subtasks => "subtasks",
            Self::Buckets => "buckets",
            Self::Reactions => "reactions",
            Self::Comments => "comments",
        })
    }
}

/// Filter, sort, search and expansion options for the task endpoints.
///
/// `page` and `per_page` are deliberately *not* here: pagination is the paginator's job,
/// and letting a caller pin a page size is how a client ends up believing it asked for
/// everything.
#[derive(Debug, Clone, Default)]
pub struct TaskQuery {
    search: Option<String>,
    filter: Option<String>,
    filter_timezone: Option<String>,
    include_nulls: bool,
    sort: Vec<(String, Order)>,
    expand: Vec<Expand>,
}

impl TaskQuery {
    /// An unfiltered query: every task the user can see.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Full-text search over task titles and descriptions (the `s` parameter).
    #[must_use]
    pub fn search(mut self, text: impl Into<String>) -> Self {
        self.search = Some(text.into());
        self
    }

    /// A Vikunja filter expression, e.g. `done = false && due_date < now/w+1w`.
    ///
    /// Passed through verbatim; the syntax is the server's, documented at
    /// <https://vikunja.io/docs/filters>.
    #[must_use]
    pub fn filter(mut self, expression: impl Into<String>) -> Self {
        self.filter = Some(expression.into());
        self
    }

    /// The IANA timezone relative dates in the filter are resolved against.
    ///
    /// Without it the server uses its own, so `due_date < now/d+1d` means "tomorrow" in
    /// the server's zone rather than the user's.
    #[must_use]
    pub fn filter_timezone(mut self, tz: impl Into<String>) -> Self {
        self.filter_timezone = Some(tz.into());
        self
    }

    /// Include tasks whose filtered field is null.
    #[must_use]
    pub fn include_nulls(mut self, include: bool) -> Self {
        self.include_nulls = include;
        self
    }

    /// Add a sort key. Call repeatedly for a multi-key sort; order is significant.
    #[must_use]
    pub fn sort(mut self, field: impl Into<String>, order: Order) -> Self {
        self.sort.push((field.into(), order));
        self
    }

    /// Ask the server for additional data on each task.
    #[must_use]
    pub fn expand(mut self, what: Expand) -> Self {
        if !self.expand.contains(&what) {
            self.expand.push(what);
        }
        self
    }

    /// Whether this query can return more tasks per page than `per_page` allows.
    #[must_use]
    pub fn may_exceed_page_size(&self) -> bool {
        self.expand.contains(&Expand::Subtasks)
    }

    /// Render as query-string pairs, in the order Vikunja expects them.
    ///
    /// `sort_by` and `order_by` are repeated parameters whose positions correspond, so
    /// they are emitted as two runs rather than interleaved.
    #[must_use]
    pub fn pairs(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        if let Some(search) = &self.search {
            pairs.push(("s".to_string(), search.clone()));
        }
        if let Some(filter) = &self.filter {
            pairs.push(("filter".to_string(), filter.clone()));
        }
        if let Some(tz) = &self.filter_timezone {
            pairs.push(("filter_timezone".to_string(), tz.clone()));
        }
        if self.include_nulls {
            // Documented as a string of "true"/"false", not a bool.
            pairs.push(("filter_include_nulls".to_string(), "true".to_string()));
        }
        for (field, _) in &self.sort {
            pairs.push(("sort_by".to_string(), field.clone()));
        }
        for (_, order) in &self.sort {
            pairs.push(("order_by".to_string(), order.to_string()));
        }
        for expand in &self.expand {
            pairs.push(("expand".to_string(), expand.to_string()));
        }
        pairs
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_sends_nothing() {
        assert!(TaskQuery::new().pairs().is_empty());
    }

    #[test]
    fn sort_keys_are_emitted_as_two_corresponding_runs() {
        let pairs = TaskQuery::new()
            .sort("due_date", Order::Asc)
            .sort("priority", Order::Desc)
            .pairs();
        assert_eq!(
            pairs,
            vec![
                ("sort_by".to_string(), "due_date".to_string()),
                ("sort_by".to_string(), "priority".to_string()),
                ("order_by".to_string(), "asc".to_string()),
                ("order_by".to_string(), "desc".to_string()),
            ]
        );
    }

    #[test]
    fn a_filter_is_passed_through_verbatim() {
        let pairs = TaskQuery::new()
            .filter("done = false && due_date < now/w+1w")
            .filter_timezone("America/New_York")
            .pairs();
        assert_eq!(pairs[0].1, "done = false && due_date < now/w+1w");
        assert_eq!(pairs[1].1, "America/New_York");
    }

    #[test]
    fn expanding_subtasks_is_flagged_as_breaking_the_page_size() {
        assert!(!TaskQuery::new().may_exceed_page_size());
        assert!(TaskQuery::new()
            .expand(Expand::Subtasks)
            .may_exceed_page_size());
        assert!(!TaskQuery::new()
            .expand(Expand::Comments)
            .may_exceed_page_size());
    }

    #[test]
    fn expansions_do_not_duplicate() {
        let query = TaskQuery::new()
            .expand(Expand::Buckets)
            .expand(Expand::Buckets);
        assert_eq!(query.pairs().len(), 1);
    }
}
