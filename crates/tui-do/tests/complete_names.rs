//! Dynamic shell completion: the binary run the way a shell runs it on Tab, with
//! `TUI_DO_COMPLETE` naming the shell and the line after `--`.
//!
//! Each test sets a scratch config *and* a scratch store, never one alone. Nothing here
//! reaches a network: completion never opens one, and the config names a loopback port
//! nothing listens on.
// Only with the feature: without it `TUI_DO_COMPLETE` is an ordinary variable.
#![cfg(feature = "dynamic-completion")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tui_do_core::models::{Label, LabelId, Project, ProjectId};
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

async fn seeded(name: &str) -> PathBuf {
    let dir = config_only(name);
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
            Project {
                id: ProjectId(3),
                title: "Two  Spaces".to_string(),
                ..Project::default()
            },
        ])
        .await
        .unwrap();
    store
        .upsert_labels(vec![Label {
            id: LabelId(1),
            title: "urgent".to_string(),
            ..Label::default()
        }])
        .await
        .unwrap();
    dir
}

/// Ask for completions as `shell` would, for the last word of `line`.
fn complete(dir: &Path, shell: &str, line: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tui-do"));
    command
        .env("TUI_DO_CONFIG", dir.join("config.yaml"))
        .env("TUI_DO_DB", dir.join("tui-do.db"))
        .env("TUI_DO_COMPLETE", shell)
        .arg("--")
        .args(line)
        .stdin(Stdio::null());
    if shell != "fish" {
        // bash and zsh say which word is under the cursor; fish passes it last.
        command.env("_CLAP_COMPLETE_INDEX", (line.len() - 1).to_string());
    }
    command.output().expect("the binary runs")
}

/// The candidates, one per line whatever separator the shell asked for, without the
/// descriptions zsh and fish carry after a `:` or a tab.
fn candidates(output: &Output) -> Vec<String> {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout)
        .split(['\n', '\u{b}'])
        .filter(|line| !line.is_empty())
        .map(|line| line.split('\t').next().unwrap_or(line).to_string())
        .collect()
}

#[tokio::test]
async fn a_project_completes_from_the_store_in_each_shell() {
    let dir = seeded("complete-shells").await;
    for shell in ["bash", "zsh", "fish"] {
        let line = ["tui-do", "add", "Call", "the", "VA", "+Le"];
        assert_eq!(
            candidates(&complete(&dir, shell, &line)),
            ["+Legal"],
            "{shell}"
        );
        let line = ["tui-do", "add", "x", "*ur"];
        assert_eq!(
            candidates(&complete(&dir, shell, &line)),
            ["*urgent"],
            "{shell}"
        );
        let line = ["tui-do", "add", "x", "due", "tomo"];
        assert_eq!(
            candidates(&complete(&dir, shell, &line)),
            ["tomorrow"],
            "{shell}"
        );
    }
    // A name with a space is offered in the bracket form. zsh and fish quote what they
    // are handed; bash inserts it as given, so for bash it comes escaped.
    let line = ["tui-do", "add", "x", "+Lif"];
    for shell in ["zsh", "fish"] {
        let found = candidates(&complete(&dir, shell, &line));
        assert_eq!(found, ["+[Life Admin]"], "{shell}");
    }
    assert_eq!(
        candidates(&complete(&dir, "bash", &line)),
        [r"+\[Life\ Admin\]"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two spaces in a row survive only as one word: `add` joins its words with one space, so
/// a name split across two would be filed under `Two Spaces`, which does not exist.
#[tokio::test]
async fn bash_passes_a_two_space_name_as_one_argument() {
    let dir = seeded("complete-two-spaces").await;
    let line = ["tui-do", "add", "x", "+Tw"];
    let found = candidates(&complete(&dir, "bash", &line));
    assert_eq!(found, [r"+\[Two\ \ Spaces\]"]);
    // What bash makes of that word once it is on the command line.
    let argv = Command::new("bash")
        .args(["--norc", "-c"])
        .arg(format!("printf '<%s>\\n' {}", found[0]))
        .output()
        .expect("bash runs");
    assert_eq!(String::from_utf8_lossy(&argv.stdout), "<+[Two  Spaces]>\n");
    for shell in ["zsh", "fish"] {
        let found = candidates(&complete(&dir, shell, &line));
        assert_eq!(found, ["+[Two  Spaces]"], "{shell}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn completion_leaves_the_store_as_it_was() {
    let dir = seeded("complete-untouched").await;
    let listing = |dir: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = std::fs::read(dir.join("tui-do.db")).unwrap();
    let files = listing(&dir);
    let output = complete(&dir, "zsh", &["tui-do", "add", "+L"]);
    assert_eq!(candidates(&output), ["+Legal", "+[Life Admin]"]);
    assert_eq!(std::fs::read(dir.join("tui-do.db")).unwrap(), before);
    assert_eq!(listing(&dir), files);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_store_gives_no_candidates_and_creates_none() {
    let dir = config_only("complete-missing");
    for shell in ["bash", "zsh", "fish"] {
        let output = complete(&dir, shell, &["tui-do", "add", "x", "+Le"]);
        assert!(candidates(&output).is_empty(), "{shell}: {output:?}");
        assert!(output.stderr.is_empty(), "{shell}: {output:?}");
    }
    assert!(
        !dir.join("tui-do.db").exists(),
        "completing created a store"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_registration_script_names_the_variable() {
    let dir = config_only("complete-registration");
    for shell in ["bash", "zsh", "fish"] {
        let output = Command::new(env!("CARGO_BIN_EXE_tui-do"))
            .env("TUI_DO_CONFIG", dir.join("config.yaml"))
            .env("TUI_DO_DB", dir.join("tui-do.db"))
            .env("TUI_DO_COMPLETE", shell)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{shell}: {output:?}");
        let script = String::from_utf8_lossy(&output.stdout);
        assert!(script.contains("TUI_DO_COMPLETE"), "{shell}: {script}");
    }
    assert!(!dir.join("tui-do.db").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_word_that_matches_nothing_gets_silence() {
    let dir = seeded("complete-silent").await;
    for shell in ["bash", "zsh", "fish"] {
        for line in [
            &["tui-do", "add", "x", "+Nowhere"][..],
            &["tui-do", "add", "x", "*nolabel"][..],
            &["tui-do", "add", "x", "due", "zzz"][..],
            &["tui-do", "add", "Call", "the"][..],
        ] {
            let output = complete(&dir, shell, line);
            assert!(output.status.success(), "{shell} {line:?}: {output:?}");
            assert!(output.stdout.is_empty(), "{shell} {line:?}: {output:?}");
            assert!(output.stderr.is_empty(), "{shell} {line:?}: {output:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A shell runs this on every Tab, so it has to be quick against a store the size of a
/// real one: dev holds about 3,900 tasks. Measured 2026-11-08 on sw-x1, debug build, the
/// whole process -- spawn, read-only open, three reads, print -- against 4,000 tasks, 100
/// projects and 50 users: the slowest of five was 15-39 ms over three runs alone, and 35 ms
/// beside the other tests. The 200 ms bound leaves room for a loaded CI box.
#[tokio::test]
async fn completion_is_quick_against_a_full_store() {
    use tui_do_core::models::{Task, TaskId, User, UserId};
    let dir = config_only("complete-timing");
    let store = Store::open(dir.join("tui-do.db")).await.unwrap();
    store
        .upsert_projects(
            (1..=100)
                .map(|id| Project {
                    id: ProjectId(id),
                    title: format!("Project {id}"),
                    ..Project::default()
                })
                .collect(),
        )
        .await
        .unwrap();
    store
        .upsert_tasks(
            (1..=4000)
                .map(|id| Task {
                    id: TaskId(id),
                    project_id: ProjectId(id % 100 + 1),
                    title: format!("Task {id}"),
                    assignees: vec![User {
                        id: UserId(id % 50 + 1),
                        username: format!("user{}", id % 50 + 1),
                        ..User::default()
                    }],
                    ..Task::default()
                })
                .collect(),
        )
        .await
        .unwrap();
    drop(store);

    let line = ["tui-do", "add", "x", "@user4"];
    // Once to warm the page cache, then the slowest of five.
    let _ = complete(&dir, "bash", &line);
    let slowest = (0..5)
        .map(|_| {
            let started = std::time::Instant::now();
            let output = complete(&dir, "bash", &line);
            let took = started.elapsed();
            assert!(!candidates(&output).is_empty(), "{output:?}");
            took
        })
        .max()
        .unwrap();
    assert!(
        slowest < std::time::Duration::from_millis(200),
        "completion took {slowest:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
