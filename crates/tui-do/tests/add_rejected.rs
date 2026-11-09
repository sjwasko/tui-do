//! `tui-do add` when the server refuses the task (BUG-35), and the limits it checks first.
//!
//! The binary runs against a scratch config *and* a scratch store, both set, never one,
//! and against a wiremock server on loopback, so nothing here can reach a real network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use tui_do_core::models::{Project, ProjectId};
use tui_do_core::store::{TaskFilter, TaskSort};
use tui_do_core::Store;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A scratch directory whose config points at `url`, with a store holding one project.
async fn scratch(name: &str, url: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tui-do-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.yaml"),
        format!("server:\n  url: {url}\n  token_file: token\n"),
    )
    .unwrap();
    std::fs::write(dir.join("token"), "tk_not_a_real_token\n").unwrap();
    // Owner-only, or every run warns about it on stderr.
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

/// Run the binary off the test's own runtime, which the mock server needs to answer.
async fn tui_do(dir: &Path, args: &[&str]) -> Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_tui-do"));
    command
        .env("TUI_DO_CONFIG", dir.join("config.yaml"))
        .env("TUI_DO_DB", dir.join("tui-do.db"))
        .args(args)
        .stdin(Stdio::null());
    tokio::task::spawn_blocking(move || command.output().expect("the binary runs"))
        .await
        .unwrap()
}

/// A JSON answer, without a JSON dependency for two fixed bodies.
fn json(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

async fn tasks_in(dir: &Path) -> usize {
    let store = Store::open(dir.join("tui-do.db")).await.unwrap();
    store
        .tasks(TaskFilter::default(), TaskSort::default())
        .await
        .unwrap()
        .len()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_the_server_refuses_is_an_error_not_sent() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects/1/tasks"))
        .respond_with(json(
            412,
            r#"{"code":2002,"message":"The task title may be at most 1000 characters long.","invalid_fields":["title"]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let dir = scratch("add-refused", &server.uri()).await;

    let output = tui_do(&dir, &["add", "Call the dentist +Personal"]).await;
    let (out, err) = (text(&output.stdout), text(&output.stderr));

    assert!(!output.status.success(), "{out}\n{err}");
    assert!(
        !out.contains("Sent."),
        "a refused task was reported sent: {out}"
    );
    assert!(
        err.contains("refused") && err.contains("at most 1000 characters"),
        "the server's reason should be passed on: {err}"
    );
    // The refusal rolled the task back, so nothing is left to send again.
    assert_eq!(tasks_in(&dir).await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_the_server_takes_is_still_sent() {
    // The other half, so the test above cannot pass by failing every add.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/projects/1/tasks"))
        .respond_with(json(
            201,
            r#"{"id":41,"project_id":1,"title":"Call the dentist"}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let dir = scratch("add-taken", &server.uri()).await;

    let output = tui_do(&dir, &["add", "Call the dentist +Personal"]).await;
    let out = text(&output.stdout);
    assert!(output.status.success(), "{out}\n{}", text(&output.stderr));
    assert!(out.contains("Sent."), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_line_past_the_servers_limits_is_refused_before_anything_is_queued() {
    // Nothing is mounted: a request reaching the server here would be the bug.
    let server = MockServer::start().await;
    let dir = scratch("add-limits", &server.uri()).await;
    let long = "a".repeat(1_001);

    for line in [long.as_str(), "water the cactus every 1000 years"] {
        for dry in [true, false] {
            let args: Vec<&str> = if dry {
                vec!["add", "--dry-run", line]
            } else {
                vec!["add", line]
            };
            let output = tui_do(&dir, &args).await;
            let err = text(&output.stderr);
            assert!(!output.status.success(), "dry={dry}: {err}");
            assert!(err.contains("not added"), "dry={dry}: {err}");
        }
    }
    assert_eq!(tasks_in(&dir).await, 0);
    assert!(server.received_requests().await.unwrap().is_empty());
}
