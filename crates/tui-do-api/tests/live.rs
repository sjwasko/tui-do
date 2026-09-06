//! Integration tests against a real Vikunja.
//!
//! These are the tests wiremock cannot replace. The `PUT`-versus-`POST` bug in
//! `seed-from-prod.sh` survived to runtime precisely because nothing exercised the real
//! server, and a mock will happily agree with whatever the client believes.
//!
//! # Running them
//!
//! ```sh
//! export TUI_DO_TEST_URL=https://your-dev-instance.example:8443   # dev, never prod
//! export TUI_DO_TEST_TOKEN=tk_...          # Settings -> API tokens, in the web UI
//! # or, instead of a token:
//! export TUI_DO_TEST_USERNAME=... TUI_DO_TEST_PASSWORD=...
//! cargo test -p tui-do-api --test live -- --nocapture
//! ```
//!
//! Without `TUI_DO_TEST_URL` every test here reports success without doing anything, so
//! `cargo test --workspace` stays green offline and in CI.
//!
//! # Prod is not a test target
//!
//! [`target`] takes an **allow**-list, not a deny-list: the URL must name a known dev
//! host, and the prod hostname is refused outright with no environment variable that can
//! switch the check off. A deny-list would have been one empty `TUI_DO_PROD_DENY=` away
//! from running the create/update/delete round-trip against production.
//!
//! `TUI_DO_DEV_HOST` names *your* dev instance; it cannot be used to permit prod.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic
)]

use std::collections::BTreeSet;

use chrono::Timelike;
use tui_do_api::models::Login;
use tui_do_api::{Client, Credentials, TaskQuery};

/// The dev instance these tests are written against, as a digest of its hostname.
///
/// `TUI_DO_DEV_HOST` names it in plain text when your dev instance lives somewhere else.
const DEV_HOST_DIGESTS: &[u64] = &[0x7bfd_740f_9b2c_1455];

/// Hosts that are never a test target, whatever else is configured, as digests.
///
/// Not overridable, deliberately. Every environment variable here is one an accident can
/// set.
const FORBIDDEN_HOST_DIGESTS: &[u64] = &[0xd46d_34ba_49c7_e934];

/// The same machine, by address. A name is not the only way to reach it.
///
/// The binary's own guard had this hole and was fixed on 2026-08-31 (BUG-6): a URL naming
/// production by IP passed a check that only looked for the hostname. This guard is the
/// same shape and had the same hole, and a live *test* suite pointed at production is the
/// worse of the two, because it writes without anybody watching.
const FORBIDDEN_ADDR_DIGESTS: &[u64] = &[0xf44a_8a55_290f_bec7, 0xe71f_ea92_f3dc_2f65];

/// FNV-1a, 64-bit — the same function and the same reasoning as the binary's own guard.
///
/// Digests rather than names so the repository does not publish the fleet's naming. That
/// is obfuscation, not secrecy: a hostname is low-entropy and falls to a dictionary attack.
/// It keeps the names out of grep and code search, which is all it is for.
const fn digest(value: &str) -> u64 {
    let bytes = value.as_bytes();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// The host of a URL: lowercased, without scheme, port, path or trailing dot.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or(rest);
    let host = host.split(':').next().unwrap_or(host);
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Whether a host, or its first label, digests to one of `digests`.
fn matches(host: &str, digests: &[u64]) -> bool {
    if host.is_empty() {
        return false;
    }
    let first_label = host.split('.').next().unwrap_or(host);
    digests.contains(&digest(host)) || digests.contains(&digest(first_label))
}

/// Projects the round-trip test creates, and the title it sweeps up before starting.
const FIXTURE_PROJECT_TITLE: &str = "tui-do live test";

/// The server under test, or `None` when these tests are not configured to run.
///
/// # Panics
/// If the configured URL is not a known dev host. That is a misconfiguration to fix, not
/// a condition to skip past — skipping would let a typo mean "ran against something
/// else", which is the failure mode worth being loud about.
fn target() -> Option<String> {
    let url = std::env::var("TUI_DO_TEST_URL").ok()?;
    let url = url.trim().to_string();
    if url.is_empty() {
        return None;
    }

    let host = host_of(&url);
    assert!(
        !matches(&host, FORBIDDEN_HOST_DIGESTS) && !matches(&host, FORBIDDEN_ADDR_DIGESTS),
        "TUI_DO_TEST_URL points at {url}, which is production. There is no way to \
         override this; point it at the dev instance."
    );

    let known_dev = match std::env::var("TUI_DO_DEV_HOST") {
        Ok(named) if !named.trim().is_empty() => host == named.trim().to_ascii_lowercase(),
        _ => matches(&host, DEV_HOST_DIGESTS),
    };
    assert!(
        known_dev,
        "TUI_DO_TEST_URL is {url}, which is not a known dev host. These tests create and \
         delete data; they run against a known dev instance or not at all. Set \
         TUI_DO_DEV_HOST if your dev instance lives somewhere else."
    );
    Some(url)
}

/// Delete anything a previous run left behind.
///
/// The round-trip test cleans up after itself on the happy path, but an assertion failure
/// between creating a project and deleting it orphans the fixtures — and that is exactly
/// the case the test exists to produce. Sweeping at the start keeps reruns deterministic
/// and stops a leftover fixture from being picked as "the first real project" by another
/// test.
async fn sweep_fixtures(client: &Client) {
    let Ok(projects) = client.all_projects().await else {
        return;
    };
    for project in projects
        .iter()
        .filter(|p| p.id.get() > 0 && p.title == FIXTURE_PROJECT_TITLE)
    {
        match client.delete_project(project.id).await {
            Ok(()) => println!("swept leftover fixture project {}", project.id),
            Err(err) => println!("could not sweep project {}: {err}", project.id),
        }
    }

    let Ok(labels) = client.all_labels().await else {
        return;
    };
    // Prefix, not equality: the round-trip renames the fixture label to prove the rename
    // works, and a run that dies between the rename and the delete leaves it behind under
    // the new title.
    for label in labels
        .iter()
        .filter(|l| l.title.starts_with("tui-do-live-test"))
    {
        match client.delete_label(label.id).await {
            Ok(()) => println!("swept leftover fixture label {}", label.id),
            Err(err) => println!("could not sweep label {}: {err}", label.id),
        }
    }
}

/// A client authenticated the way the environment says, or `None` to skip.
async fn connect(test: &str) -> Option<Client> {
    let Some(url) = target() else {
        println!("skipping {test}: set TUI_DO_TEST_URL to the dev instance to run it");
        return None;
    };

    let token = std::env::var("TUI_DO_TEST_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let username = std::env::var("TUI_DO_TEST_USERNAME")
        .ok()
        .filter(|u| !u.is_empty());

    let mut builder = Client::builder(&url);
    if let Some(token) = token {
        builder = builder.credentials(Credentials::api_token(token));
    }
    let client = builder
        .build()
        .expect("TUI_DO_TEST_URL should be a valid URL");

    // Reading /info first is what a real startup does, and it is what tells the paginators
    // the server's page cap instead of leaving them on the built-in default.
    let info = client.info().await.expect("GET /info should succeed");
    println!(
        "{test}: {url} is Vikunja {} (cap {})",
        info.version,
        info.page_cap()
    );

    if let Some(username) = username {
        let password = std::env::var("TUI_DO_TEST_PASSWORD")
            .expect("TUI_DO_TEST_USERNAME needs TUI_DO_TEST_PASSWORD");
        client
            .login(&Login::new(username, password))
            .await
            .expect("POST /login should succeed");
    }

    assert_ne!(
        client.auth_kind(),
        tui_do_api::AuthKind::Anonymous,
        "set TUI_DO_TEST_TOKEN, or TUI_DO_TEST_USERNAME and TUI_DO_TEST_PASSWORD"
    );

    sweep_fixtures(&client).await;
    Some(client)
}

#[tokio::test]
async fn info_reports_the_version_the_spec_was_captured_from() {
    let Some(url) = target() else {
        println!("skipping: TUI_DO_TEST_URL is not set");
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

    // Does a list endpoint populate assignees, or leave them nil? It decides how
    // dangerous read-mutate-write is: `update_task` sends the whole body and an empty
    // `assignees` clears them, so if list results omit assignees then every optimistic
    // edit unassigns everyone. Reported rather than asserted -- seeded data may
    // legitimately have none, and a zero here means "inconclusive", not "broken".
    let with_assignees = tasks.iter().filter(|t| !t.assignees.is_empty()).count();
    let with_labels = tasks.iter().filter(|t| !t.labels.is_empty()).count();
    println!(
        "of {} tasks from the list endpoint, {with_assignees} carry assignees and \
         {with_labels} carry labels",
        tasks.len()
    );

    // The lower bound implied by the page count: every page but the last was full.
    assert!(tasks.len() >= ((total_pages - 1) * cap + 1) as usize);

    if let Ok(expected) = std::env::var("TUI_DO_TEST_EXPECTED_TASKS") {
        let expected: usize = expected
            .parse()
            .expect("TUI_DO_TEST_EXPECTED_TASKS is a number");
        assert_eq!(tasks.len(), expected);
    }
}

#[tokio::test]
async fn an_unfiltered_task_listing_includes_done_tasks() {
    // The premise the sync engine's pull rests on. It stores what `GET /tasks` returns
    // and then deletes every local task the listing did not mention, so if the server
    // quietly omits completed tasks, a single pull erases all of them -- and the user
    // sees their history disappear rather than an error.
    //
    // Differential rather than absolute: ask for done tasks explicitly, and check the
    // unfiltered listing already contained them. That works whatever the seeded data
    // happens to hold, and says so when it holds nothing conclusive.
    let Some(client) = connect("an_unfiltered_task_listing_includes_done_tasks").await else {
        return;
    };

    let unfiltered = client.all_tasks(&TaskQuery::new()).await.expect("fetch");
    let done_query = TaskQuery::new().filter("done = true");
    let done = client.all_tasks(&done_query).await.expect("fetch done");

    println!(
        "{} tasks unfiltered, of which {} are done; {} returned by `done = true`",
        unfiltered.len(),
        unfiltered.iter().filter(|t| t.done).count(),
        done.len()
    );

    if done.is_empty() {
        println!(
            "inconclusive: the dev instance has no completed tasks. Complete one and \
             re-run to settle this."
        );
        return;
    }

    let listed: BTreeSet<i64> = unfiltered.iter().map(|t| t.id.get()).collect();
    let missing: Vec<i64> = done
        .iter()
        .map(|t| t.id.get())
        .filter(|id| !listed.contains(id))
        .collect();
    assert!(
        missing.is_empty(),
        "the unfiltered listing omitted {} completed task(s) that `done = true` returns \
         ({missing:?}). The sync engine's pull would delete them from the local store; \
         it must pull done tasks explicitly instead.",
        missing.len()
    );
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
    // Vikunja returns pseudo-projects in this list: -1 is Favorites, and saved filters
    // appear with negative ids too. They reject writes, so a project picker has to
    // exclude them -- this is where the count of them gets noticed.
    let (pseudo, real): (Vec<_>, Vec<_>) = projects.iter().partition(|p| p.id.get() < 0);
    println!(
        "{} projects: {} real, {} pseudo ({:?})",
        projects.len(),
        real.len(),
        pseudo.len(),
        pseudo.iter().map(|p| (p.id, &p.title)).collect::<Vec<_>>()
    );
    assert!(!projects.is_empty());

    // Labels may legitimately be empty on a sparse instance; loading them must still work.
    let labels = client.all_labels().await.expect("labels should load");
    println!("{} labels", labels.len());

    // Does `/labels` cover labels that only appear on someone else's task? The spec says
    // it returns labels "either created by the user or associated with a task the user
    // has at least read-access to", and the sync engine's pull trusts that: it retains
    // the stored labels against this listing, and a label dropped there takes its
    // `task_labels` links with it. Reported rather than asserted -- a seed where every
    // labelled task uses a listed label is inconclusive, not broken.
    let tasks = client.all_tasks(&TaskQuery::new()).await.expect("fetch");
    let listed: BTreeSet<i64> = labels.iter().map(|l| l.id.get()).collect();
    let on_tasks: BTreeSet<i64> = tasks
        .iter()
        .flat_map(|t| t.labels.iter().map(|l| l.id.get()))
        .collect();
    let unlisted: Vec<i64> = on_tasks.difference(&listed).copied().collect();
    println!(
        "labels seen on tasks: {:?}; listed by /labels: {:?}; on tasks but not listed: \
         {unlisted:?}",
        on_tasks, listed
    );

    let first = real.first().copied().expect("at least one real project");
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

/// A task update replaces the task's reminders from the request body.
///
/// Measured here rather than read out of the spec, because the spec does not say: it
/// marks `attachments` and `labels` read-only and says nothing of the sort about
/// `reminders` — the same shape as `assignees`, which is known to be body-replaced.
///
/// This cost a real bug. Nothing in tui-do stored reminders, so a task read back out of
/// the store carried an empty `Vec<TaskReminder>`, and `Task` serialises every field —
/// meaning every optimistic edit sent `"reminders": []` and silently deleted whatever the
/// user had set. Renaming a task destroyed its reminders. The store now keeps them
/// (`task_reminders`, schema v3) so the body carries them back.
///
/// Both halves are asserted: the destructive shape, so a change in Vikunja's behaviour is
/// noticed rather than assumed, and the safe one, which is what tui-do now does.
#[tokio::test]
async fn a_task_update_replaces_reminders_from_the_body() {
    let Some(client) = connect("a_task_update_replaces_reminders_from_the_body").await else {
        return;
    };

    let project = client
        .create_project(&tui_do_api::models::Project {
            title: FIXTURE_PROJECT_TITLE.into(),
            description: "created by cargo test -p tui-do-api --test live".into(),
            ..Default::default()
        })
        .await
        .expect("PUT /projects should create a project");

    let due = chrono::Utc::now() + chrono::Duration::days(3);
    let remind_at = (due - chrono::Duration::hours(2))
        .with_nanosecond(0)
        .expect("truncating to whole seconds is always valid");
    let created = client
        .create_task(
            project.id,
            &tui_do_api::models::Task {
                title: "reminder probe".into(),
                due_date: Some(due).into(),
                reminders: vec![tui_do_api::models::TaskReminder {
                    reminder: Some(remind_at).into(),
                    relative_period: 0,
                }],
                ..Default::default()
            },
        )
        .await
        .expect("PUT /projects/{id}/tasks should create a task");

    // If the server did not take the reminder on create, the experiment cannot run --
    // say so rather than passing quietly, the same rule the done-task test uses.
    let stored = client.task(created.id).await.expect("GET /tasks/{id}");
    if stored.reminders.is_empty() {
        println!(
            "inconclusive: the server did not store a reminder sent on create, so this \
             cannot measure what an update does to one"
        );
        client.delete_project(project.id).await.ok();
        return;
    }
    println!("task created with {} reminder(s)", stored.reminders.len());

    // Half one: carrying the reminders back preserves them. This is what tui-do does now
    // that the store keeps them, and it is the assertion that matters for correctness.
    let mut kept = stored.clone();
    kept.title = "reminder probe, renamed".into();
    let after_safe_edit = client
        .update_task(&kept)
        .await
        .expect("POST /tasks/{id} should update");
    println!(
        "an update carrying {} reminder(s) answered with {}",
        kept.reminders.len(),
        after_safe_edit.reminders.len()
    );
    let reread = client.task(created.id).await.expect("GET /tasks/{id}");
    let survived = !reread.reminders.is_empty();

    // Half two: sending an empty list clears them. Documented so that the day Vikunja
    // stops doing this, the reason `task_reminders` exists is re-examined rather than
    // quietly kept.
    let mut emptied = reread.clone();
    emptied.reminders = Vec::new();
    client
        .update_task(&emptied)
        .await
        .expect("POST /tasks/{id} should update");
    let after_empty = client.task(created.id).await.expect("GET /tasks/{id}");
    let cleared = after_empty.reminders.is_empty();
    println!(
        "an update carrying \"reminders\": [] left {} behind",
        after_empty.reminders.len()
    );

    client.delete_project(project.id).await.ok();

    assert!(
        survived,
        "an update that carried the task's own reminders still lost them, so keeping \
         them in the store is not enough and the field needs different handling"
    );
    assert!(
        cleared,
        "sending an empty reminders list no longer clears them. That is the behaviour \
         `task_reminders` (schema v3) exists to survive -- re-check whether the store \
         still needs to carry reminders through a read-mutate-write cycle."
    );
}

/// Relations survive an update whose body carries none, unlike reminders.
///
/// The rule this settles. `reminders` cost a live bug: `POST /tasks/{id}` replaces it
/// from the body exactly as it replaces `assignees`, so a task read out of a store with
/// nowhere to keep reminders went back carrying `"reminders": []` and the server deleted
/// them — renaming a task destroyed its reminders, silently, every time. `CLAUDE.md`
/// generalised that to "any `Vec` on `Task` the spec does not mark read-only is replaced
/// from the body", and named `attachments` and `related_tasks` as the two untested cases
/// left. Both are exactly as unstored as reminders were.
///
/// Measured on dev 2026-08-30, and the generalisation is **wrong**: an update carrying
/// `related_tasks: {}` and `attachments: []` left a relation and an attachment untouched.
/// Only `reminders` and `assignees` are replaced from the body. There is no rule to
/// derive one from — each collection has to be measured, which is what this is.
///
/// Attachments are not exercised here, only relations: uploading one needs a multipart
/// request this client has no method for, and adding one to reach a test would be more
/// untested code than the test is worth. The attachment half was measured by hand the
/// same day, the same way, with the same answer.
///
/// The other half of the finding is in the assertions: the write's *response* comes back
/// with `related_tasks` empty even though the relation is still there. Nothing may read a
/// write's answer and conclude a task has no relations.
#[tokio::test]
async fn a_task_update_leaves_relations_alone() {
    let Some(client) = connect("a_task_update_leaves_relations_alone").await else {
        return;
    };

    let project = client
        .create_project(&tui_do_api::models::Project {
            title: FIXTURE_PROJECT_TITLE.into(),
            description: "created by cargo test -p tui-do-api --test live".into(),
            ..Default::default()
        })
        .await
        .expect("PUT /projects should create a project");

    let make = |title: &'static str| {
        let client = &client;
        let project = project.id;
        async move {
            client
                .create_task(
                    project,
                    &tui_do_api::models::Task {
                        title: title.into(),
                        ..Default::default()
                    },
                )
                .await
                .expect("PUT /projects/{id}/tasks should create a task")
        }
    };
    let one = make("relation probe").await;
    let other = make("the other end").await;

    client
        .relate_tasks(one.id, other.id, tui_do_api::models::RelationKind::Related)
        .await
        .expect("PUT /tasks/{id}/relations should relate them");

    // As with the done-task and reminder tests: if the setup did not take, say so rather
    // than passing quietly on an experiment that never ran.
    let before = client.task(one.id).await.expect("GET /tasks/{id}");
    if before.related_tasks.is_empty() {
        println!(
            "inconclusive: the server did not record the relation, so this cannot \
             measure what an update does to one"
        );
        client.delete_project(project.id).await.ok();
        return;
    }

    // The shape a task read back out of tui-do's store has: everything it keeps, and the
    // collections it has nowhere to put coming back empty.
    let mut renamed = before.clone();
    renamed.title = "relation probe, renamed".into();
    renamed.related_tasks.clear();
    renamed.attachments.clear();
    let answered = client
        .update_task(&renamed)
        .await
        .expect("POST /tasks/{id} should update the task");

    let after = client.task(one.id).await.expect("GET /tasks/{id}");
    let survived = !after.related_tasks.is_empty();
    let echoed = !answered.related_tasks.is_empty();
    println!(
        "relation before: {}, write answered with: {}, after: {}",
        before.related_tasks.len(),
        answered.related_tasks.len(),
        after.related_tasks.len()
    );

    client.delete_project(project.id).await.ok();

    assert!(
        survived,
        "an update carrying no relations deleted the task's relations. That is the \
         reminders bug in a second field, and the store needs somewhere to keep them \
         before anything writes a task that has any"
    );
    assert!(
        !echoed,
        "the write's answer now carries the relations it did not send. Good news, but \
         `a_task_update_leaves_relations_alone` documents the opposite -- re-check what \
         may safely be believed from a write's response"
    );
    assert_eq!(after.title, "relation probe, renamed", "the write did land");
}

/// A partial task body clears every field it omits, and a three-way merge survives it.
///
/// Two facts in one test because the second only matters given the first. Vikunja's
/// update replaces the task from the request body -- `POST /tasks/{id}` carrying only an
/// id and a title cleared the description, the priority and the due date -- and it offers
/// no conditional write: no version, no ETag, and `updated` is server-set and unwritable.
///
/// So a client cannot protect a concurrent edit by sending fewer fields, and cannot ask
/// the server to refuse a stale write. What it can do is read the current copy and replay
/// the user's change onto it, which is what `Task::merge_onto` does. This exercises that
/// against the real server with a real concurrent change in between -- the shape a fleet
/// of boxes against one Vikunja produces all day.
#[tokio::test]
async fn a_partial_write_clears_what_it_omits_and_a_merge_does_not() {
    let Some(client) = connect("a_partial_write_clears_what_it_omits_and_a_merge_does_not").await
    else {
        return;
    };

    let project = client
        .create_project(&tui_do_api::models::Project {
            title: FIXTURE_PROJECT_TITLE.into(),
            description: "created by cargo test -p tui-do-api --test live".into(),
            ..Default::default()
        })
        .await
        .expect("PUT /projects should create a project");

    let created = client
        .create_task(
            project.id,
            &tui_do_api::models::Task {
                title: "original title".into(),
                description: "keep me".into(),
                priority: 4,
                ..Default::default()
            },
        )
        .await
        .expect("PUT /projects/{id}/tasks should create a task");

    // What "box A" read, and what it wants to change: the priority, nothing else.
    let before = client.task(created.id).await.expect("GET /tasks/{id}");
    let mut after = before.clone();
    after.priority = 1;

    // "Box B" renames it in the meantime, through a full-body write of its own.
    let mut elsewhere = before.clone();
    elsewhere.title = "renamed by another box".into();
    client
        .update_task(&elsewhere)
        .await
        .expect("the concurrent rename should land");

    // Box A now writes. Sending its own stale copy would put "original title" back.
    let current = client.task(created.id).await.expect("GET /tasks/{id}");
    let merged = after.merge_onto(&before, current);
    assert!(
        merged.collisions.is_empty(),
        "the two boxes changed different fields, so this is not a collision: {:?}",
        merged.collisions
    );
    client
        .update_task(&merged.task)
        .await
        .expect("POST /tasks/{id} should update");

    let settled = client.task(created.id).await.expect("GET /tasks/{id}");
    println!(
        "after a merged write: title {:?}, priority {}, description {:?}",
        settled.title, settled.priority, settled.description
    );

    client.delete_project(project.id).await.ok();

    assert_eq!(
        settled.title, "renamed by another box",
        "the merged write reverted the other box's rename"
    );
    assert_eq!(settled.priority, 1, "the user's own change did not survive");
    assert_eq!(
        settled.description, "keep me",
        "a field neither box touched was cleared"
    );
}

#[tokio::test]
async fn a_task_round_trips_through_create_read_update_delete() {
    let Some(client) = connect("a_task_round_trips_through_create_read_update_delete").await else {
        return;
    };

    // Everything is created inside a throwaway project and deleted again, so a run leaves
    // the dev instance as it found it even without the deploy kit's reset script.
    let project = client
        .create_project(&tui_do_api::models::Project {
            title: FIXTURE_PROJECT_TITLE.into(),
            description: "created by cargo test -p tui-do-api --test live".into(),
            ..Default::default()
        })
        .await
        .expect("PUT /projects should create a project");
    assert!(project.id.get() > 0);
    println!("created project {}", project.id);

    let due = chrono::Utc::now() + chrono::Duration::days(3);
    let created = client
        .create_task(
            project.id,
            &tui_do_api::models::Task {
                title: "round trip".into(),
                description: "written by the live test".into(),
                priority: 3,
                due_date: Some(due).into(),
                ..Default::default()
            },
        )
        .await
        .expect("PUT /projects/{id}/tasks should create a task");
    assert!(created.id.get() > 0);
    assert_eq!(created.title, "round trip");
    assert_eq!(created.project_id, project.id);
    assert!(
        created.due_date.get().is_some(),
        "the due date did not survive the round trip"
    );

    let read = client.task(created.id).await.expect("GET /tasks/{id}");
    assert_eq!(read.id, created.id);
    assert_eq!(read.title, "round trip");

    // The experiment that settles whether read-mutate-write can lose assignees.
    //
    // A full fetch of the seeded data found zero tasks carrying assignees, which proves
    // nothing: nobody ever assigned anyone on this instance. So assign someone here and
    // ask a *list* endpoint what it says. If the assignment comes back, list results are
    // complete and `update_task` can safely round-trip a listed task. If it does not,
    // every optimistic edit made from a list view would unassign everyone, and
    // `Task::assignees` has to become `Option<Vec<User>>` to make that unrepresentable.
    let me = client.current_user().await.expect("GET /user");
    client
        .assign_user(created.id, me.id)
        .await
        .expect("PUT /tasks/{taskID}/assignees should assign");

    let views = client
        .project_views(project.id)
        .await
        .expect("project views");
    let view = views.first().expect("a new project has a List view");

    // This runs before the task is marked done, and that ordering is load-bearing:
    // Vikunja's default List view carries a `done = false` filter, so a completed task
    // vanishes from it. Asking the view about a done task found nothing and looked like
    // the assignees had been dropped.
    let listed = client
        .view_tasks(project.id, view.id, &TaskQuery::new())
        .expect("pager")
        .collect_all()
        .await
        .expect("view tasks should load");
    let listed_task = listed
        .iter()
        .find(|t| t.id == created.id)
        .expect("the task we created should be in its project's view");
    println!(
        "list endpoint returned {} assignee(s) for a task with one assigned",
        listed_task.assignees.len()
    );
    assert!(
        !listed_task.assignees.is_empty(),
        "a list endpoint dropped a task's assignees. update_task sends the whole body \
         and an empty assignees list clears them, so read-mutate-write from a list view \
         would unassign everyone. Task::assignees must become Option<Vec<User>> so the \
         difference between \"not populated\" and \"nobody assigned\" is representable."
    );

    client
        .unassign_user(created.id, me.id)
        .await
        .expect("DELETE /tasks/{taskID}/assignees/{userID} should unassign");

    // Update the whole task, the way an optimistic write does: read, mutate, send back.
    let mut edited = read.clone();
    edited.title = "round trip, edited".into();
    edited.done = true;
    edited.due_date = None.into();
    let updated = client
        .update_task(&edited)
        .await
        .expect("POST /tasks/{id} should update");
    assert_eq!(updated.title, "round trip, edited");
    assert!(updated.done);
    assert_eq!(
        updated.due_date.get(),
        None,
        "sending the zero time should have cleared the due date, not left it set"
    );
    assert!(
        updated.done_at.get().is_some(),
        "completing a task should stamp done_at"
    );

    // A project view is not a plain task list: it carries a filter. The default List
    // view's is `done = false`, so the task just completed drops out of it. Worth a test
    // rather than a comment -- the Kanban and filter views in Phase 6 are built on these
    // endpoints, and "the view returned fewer tasks than the project has" is going to
    // look like a bug the first time it happens.
    let after_done = client
        .view_tasks(project.id, view.id, &TaskQuery::new())
        .expect("pager")
        .collect_all()
        .await
        .expect("view tasks should load");
    assert!(
        !after_done.iter().any(|t| t.id == created.id),
        "the default List view returned a done task, so it no longer filters on \
         `done = false` -- which changes how Phase 6 has to load a project"
    );

    // Labels attach through their own endpoint, not through the task body.
    let label = client
        .create_label(&tui_do_api::models::Label {
            title: "tui-do-live-test".into(),
            hex_color: "4287f5".into(),
            ..Default::default()
        })
        .await
        .expect("PUT /labels should create a label");
    client
        .add_label_to_task(created.id, label.id)
        .await
        .expect("PUT /tasks/{task}/labels should attach");
    let attached = client.task_labels(created.id).await.expect("task labels");
    assert!(attached.iter().any(|l| l.id == label.id));

    // The two label reads the sync engine leans on, neither of which had ever run against
    // a server. `update_label` sent `PUT` for months precisely because nothing called it,
    // and a client method with no live caller is exactly where that class of bug lives.
    //
    // `GET /labels/{id}` is the read half of read-merge-write: a queued rename is replayed
    // onto whatever this returns, so a wrong body here silently reverts another box's
    // colour rather than failing.
    let read_back = client
        .label(label.id)
        .await
        .expect("GET /labels/{id} should read a label back");
    assert_eq!(read_back.id, label.id);
    assert_eq!(read_back.title, "tui-do-live-test");
    assert_eq!(
        read_back.hex_color, "4287f5",
        "a label read back does not carry the colour it was created with"
    );

    // `GET /labels?s=` plus an exact filter, which is how a retried `CreateLabel` finds
    // the label its own lost response created. Vikunja's `s` matches substrings; the
    // exactness is `labels_named`'s doing, and adopting "next week" when the user asked
    // for "next" would be worse than the duplicate it exists to avoid.
    let exact = client
        .labels_named("tui-do-live-test")
        .await
        .expect("GET /labels?s= should search");
    assert_eq!(
        exact.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![label.id],
        "the exact search found something other than the label just created"
    );

    // Case-insensitively, because Vikunja will hold `Next` and `next` and a user typing a
    // familiar name means one thing by it.
    let shouted = client
        .labels_named("TUI-DO-LIVE-TEST")
        .await
        .expect("GET /labels?s= should search");
    assert_eq!(
        shouted.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![label.id],
        "the search is case-sensitive, so a retried create will duplicate rather than adopt"
    );

    // The assertions that matter, and the ones that prove the *filter* rather than the
    // server. Each is paired with the raw `s=` listing it narrows, because on its own
    // "the exact search did not return this" passes vacuously against a server whose `s`
    // matched exactly -- and then it would be testing nothing at all. Comparing the two
    // listings is the only thing that tells `labels_named`'s work from Vikunja's.
    //
    // Direction one: a title that *contains* the search term. Created here rather than
    // assumed, so the raw listing has something to hold that the filtered one must not.
    let sibling = client
        .create_label(&tui_do_api::models::Label {
            title: "tui-do-live-test sibling".into(),
            ..Default::default()
        })
        .await
        .expect("PUT /labels should create the sibling");
    let raw = client
        .labels_matching("tui-do-live-test")
        .await
        .expect("GET /labels?s= should search");
    assert!(
        raw.iter().any(|l| l.id == sibling.id),
        "`s=` did not return the longer title, so it is not the substring search \
         `labels_named` is written against and the filter below proves nothing: {raw:?}"
    );
    let still_exact = client
        .labels_named("tui-do-live-test")
        .await
        .expect("GET /labels?s= should search");
    assert_eq!(
        still_exact.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![label.id],
        "a longer title containing the search term was returned as an exact match"
    );

    // Direction two: a prefix of the fixture's title. Same pairing -- the raw listing has
    // to reach the fixture before "the filtered one does not" says anything.
    let raw_prefix = client
        .labels_matching("tui-do-live")
        .await
        .expect("GET /labels?s= should search");
    assert!(
        raw_prefix.iter().any(|l| l.id == label.id),
        "`s=tui-do-live` did not reach the fixture, so the prefix assertion below would \
         pass without the filter doing anything: {raw_prefix:?}"
    );
    let by_prefix = client
        .labels_named("tui-do-live")
        .await
        .expect("GET /labels?s= should search");
    assert!(
        by_prefix.is_empty(),
        "a prefix of the title was returned as an exact match: {by_prefix:?}"
    );
    client
        .delete_label(sibling.id)
        .await
        .expect("delete the sibling label");

    // Three things the sync engine currently assumes, none of them checkable against a
    // mock, all of them reported rather than asserted -- the point is to learn what the
    // server does, and a failing test here would only say "it did something else".
    //
    // 1. Replaying an attach that already landed. One queued entry is one request now,
    //    but a lost response still means a retry, and a 4xx would make the engine treat
    //    the attach as refused and roll the label back off the task.
    let reattached = client.add_label_to_task(created.id, label.id).await;
    println!("attaching an already-attached label answers: {reattached:?}");

    // 2. Whether a task write echoes labels. `sync::with_labels` restores the intended
    //    set onto the server's answer on the belief that it does not -- a belief, not a
    //    measurement, and if it is wrong the restore is pointless work.
    let mut relabelled = client.task(created.id).await.expect("re-read for labels");
    relabelled.title = "round trip, edited".into();
    let echoed = client
        .update_task(&relabelled)
        .await
        .expect("POST /tasks/{id} should update");
    let held = client
        .task_labels(created.id)
        .await
        .map(|l| l.len())
        .unwrap_or_default();
    println!(
        "a task write's response carries {} label(s); the task holds {held}",
        echoed.labels.len()
    );

    client
        .remove_label_from_task(created.id, label.id)
        .await
        .expect("DELETE /tasks/{task}/labels/{label} should detach");

    // 3. That detaching a label that is already gone answers 404. `sync::is_already_done`
    //    treats exactly that as success rather than as a rejection to roll back; any
    //    other 4xx here means the engine would undo a detach the user asked for.
    let redetached = client.remove_label_from_task(created.id, label.id).await;
    println!("detaching an already-detached label answers: {redetached:?}");

    // The rest of the label lifecycle, which is what closes Phase 4's "tui-do cannot
    // create labels yet" gap. Asserted rather than reported: a failure here is a bug in
    // tui-do, not a fact about the server.
    //
    // `update_label` sent `PUT` until 2026-08-29, because that is the verb
    // `spec/vikunja.json` documents. The server answers `405 Method Not Allowed`, and
    // `OPTIONS /labels/{id}` replies `Allow: OPTIONS, DELETE, GET, POST`. Nothing called
    // the method, so nothing ever saw the 405 -- the conformance test compares paths, not
    // verbs, and cannot.
    let mut edited = label.clone();
    edited.title = "tui-do-live-test renamed".into();
    let edited = client
        .update_label(&edited)
        .await
        .expect("POST /labels/{id} should rename a label");
    assert_eq!(edited.id, label.id);
    assert_eq!(edited.title, "tui-do-live-test renamed");
    // The whole label goes in the body because a partial one clears what it omits: a body
    // carrying only `title` cleared `hex_color` to "" on dev, the same way a partial task
    // body clears description, priority and due date.
    assert_eq!(
        edited.hex_color, "4287f5",
        "the rename cleared the colour, so the body no longer carries the whole label"
    );

    client.delete_label(label.id).await.expect("delete label");

    // What a replayed create answers, and it is the awkward one: labels have no unique
    // title, so this is a second label rather than an error, and nothing in the response
    // distinguishes it from the first. Reported, because there is no arm for
    // `is_already_done` to grow -- the queue has to avoid the retry instead.
    let first = client
        .create_label(&tui_do_api::models::Label {
            title: "tui-do-live-test duplicate".into(),
            ..Default::default()
        })
        .await;
    let second = client
        .create_label(&tui_do_api::models::Label {
            title: "tui-do-live-test duplicate".into(),
            ..Default::default()
        })
        .await;
    println!("creating the same label title twice answers: {first:?} then {second:?}");
    for made in [first, second].into_iter().flatten() {
        let _ = client.delete_label(made.id).await;
    }

    // And what a rename of a label that is already gone answers, which is the arm a
    // replayed rename lands on after another box deleted the label.
    let renamed_ghost = client.update_label(&edited).await;
    println!("renaming a deleted label answers: {renamed_ghost:?}");

    // The comment endpoints, including the update the spec declares with no request body.
    let comment = client
        .create_comment(created.id, "posted by the live test")
        .await
        .expect("PUT /tasks/{taskID}/comments should post");
    assert!(comment.comment.contains("posted by the live test"));

    let mut edited_comment = comment.clone();
    edited_comment.comment = "edited by the live test".into();
    let updated_comment = client
        .update_comment(created.id, &edited_comment)
        .await
        .expect("POST /tasks/{taskID}/comments/{commentID} should update");
    assert!(
        updated_comment.comment.contains("edited"),
        "the spec declares no body for a comment update; the server ignored ours, so the \
         model or the endpoint needs revisiting"
    );

    client
        .delete_comment(created.id, comment.id)
        .await
        .expect("delete comment");
    client.delete_task(created.id).await.expect("delete task");
    client
        .delete_project(project.id)
        .await
        .expect("delete project");

    // The task is gone, and asking for it says so specifically rather than vaguely.
    match client.task(created.id).await {
        Err(err) => println!("deleted task now reports: {err}"),
        Ok(task) => panic!("the deleted task is still readable: {task:?}"),
    }
}
