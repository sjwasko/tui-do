//! Dynamic shell completion: the binary run the way a shell runs it on Tab, with
//! `TUI_DO_COMPLETE` naming the shell and the line after `--`.
//!
//! Each test sets a scratch config *and* a scratch store, never one alone. Nothing here
//! reaches a network: completion never opens one, and the config names a loopback port
//! nothing listens on.
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
    // A name with a space is offered in the bracket form, which the parser reads whole
    // even after a shell has split it into two arguments.
    let line = ["tui-do", "add", "x", "+Lif"];
    assert_eq!(
        candidates(&complete(&dir, "fish", &line)),
        ["+[Life Admin]"]
    );
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
    let output = complete(&dir, "bash", &["tui-do", "add", "+L"]);
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
