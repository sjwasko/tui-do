//! `tui-do add` with a description: `-d`, `--description-file`, `--edit`, and the reports.
//!
//! Each test runs the built binary against a scratch config *and* a scratch store, both
//! set, never one: a scratch config alone still writes to the real store. The server is
//! a loopback port nothing listens on, and every add is `--offline` besides.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tui_do_core::models::{Project, ProjectId, Task};
use tui_do_core::store::{Mutation, TaskFilter, TaskSort};
use tui_do_core::Store;

/// A scratch directory with a config that loads and a store holding one project.
async fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tui-do-desc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.yaml"),
        "server:\n  url: http://127.0.0.1:9\n  token_file: token\n",
    )
    .unwrap();
    std::fs::write(dir.join("token"), "tk_not_a_real_token\n").unwrap();
    std::fs::set_permissions(
        dir.join("token"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let store = Store::open(dir.join("tui-do.db")).await.unwrap();
    store
        .upsert_projects(vec![Project {
            id: ProjectId(1),
            title: "Personal".to_string(),
            ..Project::default()
        }])
        .await
        .unwrap();
    dir
}

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tui-do"));
    command
        .env("TUI_DO_CONFIG", dir.join("config.yaml"))
        .env("TUI_DO_DB", dir.join("tui-do.db"))
        .args(args)
        .stdin(Stdio::null());
    command
}

fn tui_do(dir: &Path, args: &[&str]) -> Output {
    command(dir, args).output().expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The tasks queued for creation, as the outbox holds them.
async fn queued(dir: &Path) -> Vec<Task> {
    let store = Store::open(dir.join("tui-do.db")).await.unwrap();
    store
        .pending(None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.mutation {
            Mutation::CreateTask { task } => Some(*task),
            _ => None,
        })
        .collect()
}

/// The one task the store's own table holds.
async fn stored(dir: &Path) -> Vec<Task> {
    let store = Store::open(dir.join("tui-do.db")).await.unwrap();
    store
        .tasks(TaskFilter::default(), TaskSort::default())
        .await
        .unwrap()
}

#[tokio::test]
async fn an_inline_description_keeps_its_newline() {
    let dir = scratch("inline").await;
    let output = tui_do(
        &dir,
        &[
            "add",
            "--offline",
            "Call the dentist +Personal",
            "-d",
            "Ask about the crown.\nBring the insurance card.",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let out = stdout(&output);
    assert!(
        out.starts_with(
            "Added \"Call the dentist\" to Personal, with a description \
             (1 paragraph, 46 characters)\n"
        ),
        "{out}"
    );
    assert!(
        !out.contains("crown"),
        "the body must not be printed: {out}"
    );

    let want = "Ask about the crown.\nBring the insurance card.";
    let tasks = queued(&dir).await;
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0].description, want);
    assert_eq!(stored(&dir).await[0].description, want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_description_file_is_stored_exactly() {
    let dir = scratch("file").await;
    // Tokens, Markdown, HTML-ish text, indentation and a CRLF, as a file might carry.
    let body = "# Plan\r\n\r\nShip +Personal *urgent !5 tomorrow.\n\n    indented <b>code</b>\n\n- one\n- two\n";
    let file = dir.join("plan.md");
    std::fs::write(&file, body).unwrap();

    let output = tui_do(
        &dir,
        &[
            "add",
            "--offline",
            "Write the plan",
            "--description-file",
            file.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let out = stdout(&output);
    assert!(
        out.contains("with a description (4 paragraphs, 82 characters)"),
        "{out}"
    );

    let want =
        "# Plan\n\nShip +Personal *urgent !5 tomorrow.\n\n    indented <b>code</b>\n\n- one\n- two";
    let tasks = queued(&dir).await;
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    let task = &tasks[0];
    assert_eq!(task.description, want);
    // The tokens in the body were body: no priority, no date, no label.
    assert_eq!(task.title, "Write the plan");
    assert_eq!(task.priority, 0);
    assert!(task.due_date.get().is_none());
    assert!(task.labels.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_dry_run_shows_the_first_line_and_queues_nothing() {
    let dir = scratch("dry").await;
    let file = dir.join("long.md");
    std::fs::write(
        &file,
        "\nThe first line of a description that goes on for well over sixty characters.\n\nSecond.\n",
    )
    .unwrap();
    let output = tui_do(
        &dir,
        &[
            "add",
            "--dry-run",
            "Read it +Personal",
            "--description-file",
            file.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let out = stdout(&output);
    let mut lines = out.lines();
    assert_eq!(
        lines.next(),
        Some("Would add \"Read it\" to Personal, with a description (2 paragraphs, 86 characters)"),
        "{out}"
    );
    assert_eq!(
        lines.next(),
        Some("Description begins: \"The first line of a description that goes on for well over…\""),
        "{out}"
    );
    assert!(queued(&dir).await.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_bad_description_is_refused_before_anything_is_queued() {
    let dir = scratch("bad").await;
    let binary = dir.join("binary.bin");
    std::fs::write(&binary, b"caf\xe9").unwrap();
    let huge = dir.join("huge.log");
    std::fs::write(&huge, vec![b'x'; 64 * 1024 + 1]).unwrap();

    for (file, says) in [
        (&binary, "not UTF-8"),
        (&huge, "64 KiB"),
        (
            &dir.join("missing.md"),
            "could not open the description file",
        ),
    ] {
        let output = tui_do(
            &dir,
            &[
                "add",
                "--offline",
                "Nope +Personal",
                "--description-file",
                file.to_str().unwrap(),
            ],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{output:?}");
        assert!(stderr.contains(says), "{file:?}: {stderr}");
    }
    assert!(queued(&dir).await.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_description_flags_conflict() {
    let dir = scratch("conflict").await;
    let output = tui_do(
        &dir,
        &[
            "add",
            "--offline",
            "Both +Personal",
            "-d",
            "inline",
            "--description-file",
            "plan.md",
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    assert!(queued(&dir).await.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_description_on_stdin_is_stored_exactly() {
    use std::io::Write as _;
    let dir = scratch("stdin").await;
    // What `tui-do add 'Write the plan' --description-file - <<'EOF'` hands over: a
    // quoted heredoc, so `$HOME` and backticks arrive unexpanded, and a final newline.
    let heredoc = "## Context\n\nThe sync drops `$HOME` paths.\nSeen on sw-x1 and arm-host-1.\n\n## Tried\n\n1. Restarting\n2. `rm -rf ~/.cache/tui-do`\n\nNeither helped.\n";
    let mut child = command(
        &dir,
        &[
            "add",
            "--offline",
            "Write the plan +Personal",
            "--description-file",
            "-",
        ],
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(heredoc.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let out = stdout(&output);
    assert!(
        out.starts_with("Added \"Write the plan\" to Personal, with a description (5 paragraphs, "),
        "{out}"
    );
    // A pipe is not a terminal, so there is no Ctrl-D hint to clutter a script's stderr.
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("Ctrl-D"),
        "{output:?}"
    );

    let tasks = queued(&dir).await;
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0].description, heredoc.strip_suffix('\n').unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_empty_stdin_adds_no_description() {
    let dir = scratch("stdin-empty").await;
    let output = tui_do(
        &dir,
        &[
            "add",
            "--offline",
            "Nothing to say +Personal",
            "--description-file",
            "-",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(
        !stdout(&output).contains("with a description"),
        "{output:?}"
    );
    assert_eq!(queued(&dir).await[0].description, "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_description_on_stdin_needs_the_text_as_an_argument() {
    let dir = scratch("stdin-no-text").await;
    let output = tui_do(&dir, &["add", "--offline", "--description-file", "-"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains("--description-file - reads the description from stdin"),
        "{stderr}"
    );
    assert!(
        stderr.contains("give the task's text as an argument"),
        "{stderr}"
    );
    assert!(queued(&dir).await.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_editor_is_refused_without_a_terminal() {
    let dir = scratch("edit-no-tty").await;
    let marker = dir.join("editor-ran");
    let output = command(&dir, &["add", "--offline", "Write it +Personal", "--edit"])
        .env_remove("VISUAL")
        .env("EDITOR", format!("touch '{}'", marker.display()))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains("neither stdin nor stdout is a terminal"),
        "{stderr}"
    );
    assert!(stderr.contains("--edit opens an editor"), "{stderr}");
    assert!(!marker.exists(), "the editor was started anyway");
    assert!(queued(&dir).await.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Quote `text` for `sh`, single quotes and all.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Run `args` under a pseudo-terminal through util-linux `script`, so `--edit`'s check
/// passes and the editor gets a terminal, as it would from a person's shell. `None` if
/// there is no `script` here to do it with.
fn under_a_pty(dir: &Path, args: &[&str], editor: &str) -> Option<Output> {
    let available = Command::new("script")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        return None;
    }
    let mut line = sh_quote(env!("CARGO_BIN_EXE_tui-do"));
    for arg in args {
        line.push(' ');
        line.push_str(&sh_quote(arg));
    }
    Some(
        Command::new("script")
            .args(["-qec", &line, "/dev/null"])
            .env("TUI_DO_CONFIG", dir.join("config.yaml"))
            .env("TUI_DO_DB", dir.join("tui-do.db"))
            .env_remove("VISUAL")
            .env("EDITOR", editor)
            .stdin(Stdio::null())
            .output()
            .expect("script runs"),
    )
}

/// The whole of `--edit` through the built binary: a real `$EDITOR`, with an argument
/// in it the way `code --wait` has one, run by `sh` on a real terminal.
#[tokio::test]
async fn a_real_editor_writes_the_description() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("edit-real").await;
    let body = dir.join("body.md");
    std::fs::write(&body, "# Steps\n\n1. Draft\n2. Review\n").unwrap();
    let script = dir.join("fake editor.sh");
    // Keeps the template's header below what it writes, as a person typing on the
    // first line would, and records what it was given.
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$1\" = --wait ] || exit 3\ncp \"$2\" {seen}\n{{ cat {body}; cat \"$2\"; }} > \"$2.new\" && mv \"$2.new\" \"$2\"\n",
            seen = sh_quote(dir.join("seen").to_str().unwrap()),
            body = sh_quote(body.to_str().unwrap()),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let editor = format!("{} --wait", sh_quote(script.to_str().unwrap()));

    let Some(output) = under_a_pty(
        &dir,
        &["add", "--offline", "Write the plan +Personal", "--edit"],
        &editor,
    ) else {
        eprintln!("skipped: no util-linux `script` to make a terminal with");
        return;
    };
    let out = stdout(&output);
    assert!(output.status.success(), "{output:?}");
    assert!(
        out.contains("Added \"Write the plan\" to Personal, with a description (2 paragraphs, "),
        "{out}"
    );

    let seen = std::fs::read_to_string(dir.join("seen")).unwrap();
    assert!(seen.contains("\"Write the plan\" in Personal"), "{seen}");
    let tasks = queued(&dir).await;
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0].description, "# Steps\n\n1. Draft\n2. Review");

    // A cancelled editor adds nothing at all.
    let Some(cancelled) = under_a_pty(
        &dir,
        &["add", "--offline", "Not this one +Personal", "--edit"],
        "false",
    ) else {
        return;
    };
    assert!(!cancelled.status.success(), "{cancelled:?}");
    assert!(
        stdout(&cancelled).contains("nothing was added"),
        "{cancelled:?}"
    );
    assert_eq!(queued(&dir).await.len(), 1, "the cancelled add was queued");

    // An editor that overwrites the whole file, scissors line and all: everything it
    // wrote is kept, and stderr says so. `sh` runs `$EDITOR "$@"`, so this is
    // `printf … > <file>`.
    let Some(whole) = under_a_pty(
        &dir,
        &["add", "--offline", "Kept whole +Personal", "--edit"],
        r"printf 'whole\n# help I left\n' >",
    ) else {
        return;
    };
    // Under `script`, stderr arrives on the same terminal as stdout.
    let said = stdout(&whole);
    assert!(whole.status.success(), "{whole:?}");
    assert!(said.contains("warning:"), "{said}");
    assert!(said.contains("scissors line was removed"), "{said}");
    let tasks = queued(&dir).await;
    let kept = tasks
        .iter()
        .find(|task| task.title == "Kept whole")
        .expect("queued");
    assert_eq!(kept.description, "whole\n# help I left");
    let _ = std::fs::remove_dir_all(&dir);
}
