//! `tui-do add --tokens`: the names a line may use, for an agent to learn once.
//!
//! Each test sets a scratch config *and* a scratch store, never one alone. The config
//! names a loopback port nothing listens on, and `--tokens` contacts no server anyway.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tui_do_core::models::{Label, LabelId, Project, ProjectId, Task, TaskId, User, UserId};
use tui_do_core::Store;

fn config_only(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tui-do-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.yaml"),
        "server:\n  url: http://127.0.0.1:9\n  token_file: token\n",
    )
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

/// Every file in `dir` with its bytes, to show a run changed nothing.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

#[tokio::test]
async fn the_names_are_grouped_and_written_as_the_best_form_writes_them() {
    let dir = config_only("tokens-seeded");
    let store = Store::open(dir.join("tui-do.db")).await.unwrap();
    store
        .upsert_projects(vec![
            Project {
                id: ProjectId(1),
                title: "Legal".to_string(),
                ..Project::default()
            },
            Project {
                id: ProjectId(2),
                title: "Life Admin".to_string(),
                ..Project::default()
            },
            // A saved filter, which no task can be filed in.
            Project {
                id: ProjectId(-2),
                title: "My Open Tasks".to_string(),
                ..Project::default()
            },
        ])
        .await
        .unwrap();
    store
        .upsert_labels(vec![
            Label {
                id: LabelId(1),
                title: "urgent".to_string(),
                ..Label::default()
            },
            Label {
                id: LabelId(2),
                title: "needs review".to_string(),
                ..Label::default()
            },
        ])
        .await
        .unwrap();
    store
        .upsert_tasks(vec![Task {
            id: TaskId(1),
            project_id: ProjectId(1),
            title: "already here".to_string(),
            assignees: vec![User {
                id: UserId(7),
                username: "admin".to_string(),
                ..User::default()
            }],
            ..Task::default()
        }])
        .await
        .unwrap();
    drop(store);

    let before = snapshot(&dir);
    let output = tui_do(&dir, &["add", "--tokens"]);
    assert!(output.status.success(), "{output:?}");
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        out.starts_with(
            "projects:\n+Legal\n+[Life Admin]\n\n\
             labels:\n*[needs review]\n*urgent\n\n\
             users:\n@admin\n\n\
             syntax:\n"
        ),
        "{out}"
    );
    assert!(out.contains("  +project   +Legal"), "{out}");
    assert!(out.contains("tui-do add --dry-run"), "{out}");
    assert!(!out.contains("My Open Tasks"), "{out}");
    assert_eq!(snapshot(&dir), before, "--tokens changed the store");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_empty_store_is_an_empty_listing() {
    let dir = config_only("tokens-empty");
    drop(Store::open(dir.join("tui-do.db")).await.unwrap());
    let output = tui_do(&dir, &["add", "--tokens"]);
    assert!(output.status.success(), "{output:?}");
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        out.starts_with(
            "projects:\n(none in the local store)\n\n\
             labels:\n(none in the local store)\n\n\
             users:\n(none in the local store)\n\n\
             syntax:\n"
        ),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_store_is_an_error_and_creates_none() {
    let dir = config_only("tokens-missing");
    let output = tui_do(&dir, &["add", "--tokens"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("no local store yet at") && err.contains("run tui-do once to sync"),
        "{err}"
    );
    assert!(!dir.join("tui-do.db").exists(), "--tokens created a store");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tokens_takes_no_task() {
    let dir = config_only("tokens-conflict");
    let output = tui_do(&dir, &["add", "--tokens", "Call the VA"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!dir.join("tui-do.db").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
