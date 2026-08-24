//! Integration tests against a real Vikunja.
//!
//! These are the tests wiremock cannot replace. The `PUT`-versus-`POST` bug in
//! `seed-from-prod.sh` survived to runtime precisely because nothing exercised the real
//! server, and a mock will happily agree with whatever the client believes.
//!
//! # Running them
//!
//! ```sh
//! export CRIAX_TEST_URL=https://sw-surface.tail9803a5.ts.net:8443   # dev, never prod
//! export CRIAX_TEST_TOKEN=tk_...          # Settings -> API tokens, in the web UI
//! # or, instead of a token:
//! export CRIAX_TEST_USERNAME=... CRIAX_TEST_PASSWORD=...
//! cargo test -p criax-api --test live -- --nocapture
//! ```
//!
//! Without `CRIAX_TEST_URL` every test here reports success without doing anything, so
//! `cargo test --workspace` stays green offline and in CI.
//!
//! # Prod is not a test target
//!
//! [`target`] refuses any URL matching `CRIAX_PROD_DENY` (default: the prod hostname) and
//! there is no flag to override it. These tests only read, but a read-only test that runs
//! against prod today is a write test against prod after the next commit.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic
)]

use std::collections::BTreeSet;

use criax_api::models::Login;
use criax_api::{Client, Credentials, TaskQuery};

/// Hosts that are never a test target, unless `CRIAX_PROD_DENY` says otherwise.
const DEFAULT_PROD_DENY: &str = "sw-hp2";

/// The server under test, or `None` when these tests are not configured to run.
///
/// # Panics
/// If the configured URL names a production host. That is a misconfiguration to fix, not
/// a condition to skip past.
fn target() -> Option<String> {
    let url = std::env::var("CRIAX_TEST_URL").ok()?;
    let url = url.trim().to_string();
    if url.is_empty() {
        return None;
    }

    let deny = std::env::var("CRIAX_PROD_DENY").unwrap_or_else(|_| DEFAULT_PROD_DENY.to_string());
    assert!(
        deny.is_empty() || !url.contains(&deny),
        "CRIAX_TEST_URL points at {url}, which matches the production deny pattern {deny:?}. \
         Point it at the dev instance instead."
    );
    Some(url)
}

/// A client authenticated the way the environment says, or `None` to skip.
async fn connect(test: &str) -> Option<Client> {
    let Some(url) = target() else {
        println!("skipping {test}: set CRIAX_TEST_URL to the dev instance to run it");
        return None;
    };

    let token = std::env::var("CRIAX_TEST_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let username = std::env::var("CRIAX_TEST_USERNAME")
        .ok()
        .filter(|u| !u.is_empty());

    let mut builder = Client::builder(&url);
    if let Some(token) = token {
        builder = builder.credentials(Credentials::api_token(token));
    }
    let client = builder
        .build()
        .expect("CRIAX_TEST_URL should be a valid URL");

    // Reading /info first is what a real startup does, and it is what tells the paginators
    // the server's page cap instead of leaving them on the built-in default.
    let info = client.info().await.expect("GET /info should succeed");
    println!(
        "{test}: {url} is Vikunja {} (cap {})",
        info.version,
        info.page_cap()
    );

    if let Some(username) = username {
        let password = std::env::var("CRIAX_TEST_PASSWORD")
            .expect("CRIAX_TEST_USERNAME needs CRIAX_TEST_PASSWORD");
        client
            .login(&Login::new(username, password))
            .await
            .expect("POST /login should succeed");
    }

    assert_ne!(
        client.auth_kind(),
        criax_api::AuthKind::Anonymous,
        "set CRIAX_TEST_TOKEN, or CRIAX_TEST_USERNAME and CRIAX_TEST_PASSWORD"
    );
    Some(client)
}

#[tokio::test]
async fn info_reports_the_version_the_spec_was_captured_from() {
    let Some(url) = target() else {
        println!("skipping: CRIAX_TEST_URL is not set");
        return;
    };
    let client = Client::builder(url).build().expect("valid url");
    let info = client.info().await.expect("GET /info should succeed");

    assert_eq!(
        info.version, "v2.5.0",
        "the server was upgraded; refresh the spec with `cargo xtask fetch-spec` and \
         re-run the conformance tests before trusting anything else"
    );
    assert!(info.page_cap() > 0);
    assert!(info.supports_vikunja_file_migration());
}

#[tokio::test]
async fn a_full_task_fetch_returns_every_page() {
    let Some(client) = connect("a_full_task_fetch_returns_every_page").await else {
        return;
    };

    let cap = client.page_size();
    let mut pager = client.tasks(&TaskQuery::new()).expect("pager");
    let mut tasks = Vec::new();
    let mut pages = 0;
    while let Some(page) = pager.next_page().await.expect("each page should load") {
        pages += 1;
        tasks.extend(page.items);
    }
    let total_pages = pager
        .total_pages()
        .expect("the server should report a page count");

    println!("fetched {} tasks across {pages} pages", tasks.len());
    assert_eq!(pages, total_pages, "stopped before the server's last page");
    assert!(
        tasks.len() > cap as usize,
        "the dev instance holds thousands of tasks; getting {} back means pagination is \
         broken -- this is exactly cria's silent truncation",
        tasks.len()
    );

    let ids: BTreeSet<i64> = tasks.iter().map(|t| t.id.get()).collect();
    assert_eq!(ids.len(), tasks.len(), "the same task arrived on two pages");

    // The lower bound implied by the page count: every page but the last was full.
    assert!(tasks.len() >= ((total_pages - 1) * cap + 1) as usize);

    if let Ok(expected) = std::env::var("CRIAX_TEST_EXPECTED_TASKS") {
        let expected: usize = expected
            .parse()
            .expect("CRIAX_TEST_EXPECTED_TASKS is a number");
        assert_eq!(tasks.len(), expected);
    }
}

#[tokio::test]
async fn dateless_tasks_do_not_read_as_two_thousand_years_overdue() {
    // The zero-time bug, checked against real data rather than a fixture: Vikunja sends
    // "0001-01-01T00:00:00Z" for every unset date, and a task with no due date must come
    // back with `None`, not a year-1 timestamp.
    let Some(client) = connect("dateless_tasks_do_not_read_as_two_thousand_years_overdue").await
    else {
        return;
    };

    let tasks = client.all_tasks(&TaskQuery::new()).await.expect("fetch");
    let now = chrono::Utc::now();
    let ancient: Vec<_> = tasks
        .iter()
        .filter_map(|t| t.due_date.get().map(|d| (t.id, d)))
        .filter(|(_, due)| due.timestamp() < 0)
        .collect();
    assert!(
        ancient.is_empty(),
        "these tasks parsed their unset due date as a real one: {ancient:?}"
    );

    let dateless = tasks.iter().filter(|t| t.due_date.get().is_none()).count();
    let overdue = tasks.iter().filter(|t| t.is_overdue(now)).count();
    println!(
        "{dateless} of {} tasks have no due date; {overdue} overdue",
        tasks.len()
    );
    assert!(dateless > 0, "seeded data should contain dateless tasks");
}

#[tokio::test]
async fn projects_labels_and_views_load() {
    let Some(client) = connect("projects_labels_and_views_load").await else {
        return;
    };

    let projects = client.all_projects().await.expect("projects should load");
    println!("{} projects", projects.len());
    assert!(!projects.is_empty());

    // Labels may legitimately be empty on a sparse instance; loading them must still work.
    let labels = client.all_labels().await.expect("labels should load");
    println!("{} labels", labels.len());

    let first = projects.first().expect("at least one project");
    let views = client
        .project_views(first.id)
        .await
        .expect("project views should load");
    assert!(
        !views.is_empty(),
        "every Vikunja project has at least a List view"
    );

    // The view endpoint is how the web frontend loads a project, and the only source of
    // each task's per-view position.
    let list = views.first().expect("at least one view");
    let tasks = client
        .view_tasks(first.id, list.id, &TaskQuery::new())
        .expect("pager")
        .collect_all()
        .await
        .expect("view tasks should load");
    println!(
        "project {} view {} holds {} tasks",
        first.title,
        list.id,
        tasks.len()
    );
}

#[tokio::test]
async fn the_current_user_is_who_we_authenticated_as() {
    let Some(client) = connect("the_current_user_is_who_we_authenticated_as").await else {
        return;
    };
    let user = client
        .current_user()
        .await
        .expect("GET /user should succeed");
    assert!(!user.username.is_empty());
    println!("authenticated as {}", user.display_name());
}
