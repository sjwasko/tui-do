//! `tui-do add --dry-run`.
//!
//! Each test runs the built binary against a scratch config *and* a scratch store, both
//! set, never one: a scratch config alone still writes to the real store. The server is
//! a loopback port nothing listens on, so not even a mistake could reach a network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tui_do_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId};
use tui_do_core::store::{LabelFilter, LabelSort, TaskFilter, TaskSort};
use tui_do_core::Store;

/// A scratch directory with a config that loads, and no store yet.
fn config_only(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tui-do-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.yaml"),
        "server:\n  url: http://127.0.0.1:9\n  token_file: token\n",
    )
    .unwrap();
    std::fs::write(dir.join("token"), "tk_not_a_real_token\n").unwrap();
    // Owner-only, or every run warns about it on stderr.
    std::fs::set_permissions(
        dir.join("token"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    dir
}

/// A scratch directory with a config that loads and a store holding one project, one
/// label and one task.
async fn scratch(name: &str) -> PathBuf {
    let dir = config_only(name);
    let store = Store::open(dir.join("tui-do.db")).await.unwrap();
    store
        .upsert_projects(vec![Project {
            id: ProjectId(1),
            title: "Personal".to_string(),
            ..Project::default()
        }])
        .await
        .unwrap();
    store
        .upsert_labels(vec![Label {
            id: LabelId(1),
            title: "health".to_string(),
            ..Label::default()
        }])
        .await
        .unwrap();
    store
        .upsert_tasks(vec![Task {
            id: TaskId(1),
            project_id: ProjectId(1),
            title: "already here".to_string(),
            ..Task::default()
        }])
        .await
        .unwrap();
    dir
}

fn tui_do(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tui-do"))
        .env("TUI_DO_CONFIG", dir.join("config.yaml"))
        .env("TUI_DO_DB", dir.join("tui-do.db"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Everything a write could change: the outbox, the tasks and the labels.
async fn contents(dir: &Path) -> (Vec<String>, Vec<String>, Vec<String>) {
    let store = Store::open(dir.join("tui-do.db")).await.unwrap();
    let outbox = store
        .pending(None)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| format!("{entry:?}"))
        .collect();
    let tasks = store
        .tasks(TaskFilter::default(), TaskSort::default())
        .await
        .unwrap()
        .into_iter()
        .map(|task| format!("{} {}", task.id.get(), task.title))
        .collect();
    let labels = store
        .labels(LabelFilter::default(), LabelSort::default())
        .await
        .unwrap()
        .into_iter()
        .map(|label| format!("{} {}", label.id.get(), label.title))
        .collect();
    (outbox, tasks, labels)
}

#[tokio::test]
async fn a_dry_run_leaves_the_store_untouched() {
    let dir = scratch("dry-run-untouched").await;
    let before = contents(&dir).await;
    assert!(before.0.is_empty(), "the scratch outbox starts empty");

    let line = "Call the dentist due 2099-01-05 +Personal *health *nope !3";
    for flags in [&["--dry-run"][..], &["--dry-run", "--create-labels"][..]] {
        let mut args = vec!["add"];
        args.extend_from_slice(flags);
        args.push(line);
        let output = tui_do(&dir, &args);
        let out = stdout(&output);
        assert!(output.status.success(), "{flags:?}: {output:?}");
        assert!(
            out.starts_with("Would add \"Call the dentist\" to Personal, due Mon 5 Jan 2099\n"),
            "{flags:?}: {out}"
        );
        assert!(
            out.contains(
                "Parsed: title \"Call the dentist\" · +Personal · *health · *nope · !3 · \
                 due Mon 5 Jan 2099\n"
            ),
            "{flags:?}: {out}"
        );
        assert!(
            out.contains(
                "as typed best: Call the dentist +Personal *health *nope !3 due 2099-01-05\n"
            ),
            "{flags:?}: {out}"
        );
        assert!(
            out.contains("Dry run: nothing was queued or sent."),
            "{out}"
        );
        assert_eq!(contents(&dir).await, before, "{flags:?} changed the store");
    }

    // The label report differs by flag, as a real add's does.
    let left_off = stdout(&tui_do(&dir, &["add", "--dry-run", line]));
    assert!(
        left_off.contains("No label called nope — it would be left off."),
        "{left_off}"
    );
    let created = stdout(&tui_do(
        &dir,
        &["add", "--dry-run", "--create-labels", line],
    ));
    assert!(created.contains("Would create label nope."), "{created}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_dry_run_fails_as_a_real_add_would() {
    let dir = scratch("dry-run-fails").await;
    let before = contents(&dir).await;

    let line = "Book a table +Dinner";
    let dry = tui_do(&dir, &["add", "--dry-run", line]);
    let real = tui_do(&dir, &["add", "--offline", line]);
    assert!(!dry.status.success(), "{dry:?}");
    assert!(!real.status.success(), "{real:?}");
    assert_eq!(dry.status.code(), real.status.code());
    assert_eq!(
        String::from_utf8_lossy(&dry.stderr),
        String::from_utf8_lossy(&real.stderr)
    );
    assert!(
        String::from_utf8_lossy(&dry.stderr).contains("no project called \"Dinner\""),
        "{dry:?}"
    );
    assert!(dry.stdout.is_empty(), "{dry:?}");
    assert_eq!(contents(&dir).await, before);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dry_run_without_a_store_creates_none() {
    let dir = config_only("dry-run-no-store");
    // A store path in a directory that does not exist either: `Store::open` would have
    // made both.
    let store = dir.join("data").join("tui-do.db");
    let output = Command::new(env!("CARGO_BIN_EXE_tui-do"))
        .env("TUI_DO_CONFIG", dir.join("config.yaml"))
        .env("TUI_DO_DB", &store)
        .args(["add", "--dry-run", "Call the dentist due 2099-01-05"])
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr.contains("no local store yet"), "{stderr}");
    assert!(stderr.contains("run tui-do once to sync"), "{stderr}");
    assert!(
        !dir.join("data").exists(),
        "the dry run created the store's directory"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_dry_run_leaves_an_older_store_as_it_was() {
    let dir = scratch("dry-run-old-store").await;
    let path = dir.join("tui-do.db");
    let current = Store::open(&path)
        .await
        .unwrap()
        .schema_version()
        .await
        .unwrap();
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!("PRAGMA user_version = {}", current - 1))
            .unwrap();
    }
    let listing = |dir: &Path| {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let bytes = std::fs::read(&path).unwrap();
    let files = listing(&dir);

    let output = tui_do(&dir, &["add", "--dry-run", "Call the dentist +Personal"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains("a real run would upgrade it first"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "the store's bytes changed"
    );
    assert_eq!(listing(&dir), files, "the dry run left files behind");

    let _ = std::fs::remove_dir_all(&dir);
}
