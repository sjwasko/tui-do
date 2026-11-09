//! The built binary, started without a terminal (BUG-25).
//!
//! `tui-do < /dev/null` used to take the terminal anyway -- raw mode and the alternate
//! screen only need stdout -- and then wait forever for a key that could never arrive,
//! with Ctrl-C disabled by raw mode. It must refuse instead, promptly, and say why.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Far longer than a refusal takes. On the unfixed binary, with a terminal to take, the
/// process never exits at all, so this only bounds the failure.
const EXITS_WITHIN: Duration = Duration::from_secs(20);

/// A scratch directory holding a config that loads and a path for a store, so the run
/// can touch neither the real config nor the real store. Both are set, never one: a
/// scratch config alone still writes to the real store.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("tui-do-no-terminal-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A loopback port nothing listens on: if the interface ever did start, it could not
    // reach a server.
    std::fs::write(
        dir.join("config.yaml"),
        "server:\n  url: http://127.0.0.1:9\n  token_file: token\n",
    )
    .unwrap();
    std::fs::write(dir.join("token"), "tk_not_a_real_token\n").unwrap();
    dir
}

#[test]
fn starting_with_no_terminal_refuses_promptly() {
    let dir = scratch("interface");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tui-do"))
        .env("TUI_DO_CONFIG", dir.join("config.yaml"))
        .env("TUI_DO_DB", dir.join("tui-do.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary starts");

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > EXITS_WITHIN {
            let _ = child.kill();
            let _ = child.wait();
            panic!("tui-do was still running after {EXITS_WITHIN:?} with no terminal");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!status.success(), "a refusal must exit non-zero: {stderr}");
    assert!(
        stderr.contains("neither stdin nor stdout is a terminal"),
        "the message must say what is not a terminal: {stderr}"
    );
    assert!(
        stderr.contains("tui-do add"),
        "the message must point at the scriptable command: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing may be drawn on a stdout that is not a terminal"
    );
    assert!(
        !dir.join("tui-do.db").exists(),
        "the refusal comes before the store is opened"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `tui-do add` with no text asks for a task only at a terminal. Anywhere else it is a
/// script that forgot its argument, and gets clap's error exactly as before item 12: no
/// prompt drawn on a stdout nobody is watching, and no wait for keys that cannot come.
#[test]
fn add_with_no_text_and_no_terminal_is_still_a_usage_error() {
    let dir = scratch("add");
    let output = Command::new(env!("CARGO_BIN_EXE_tui-do"))
        .arg("add")
        .env("TUI_DO_CONFIG", dir.join("config.yaml"))
        .env("TUI_DO_DB", dir.join("tui-do.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(2),
        "clap's usage exit code: {stderr}"
    );
    assert!(
        stderr.contains("the following required arguments were not provided"),
        "{stderr}"
    );
    assert!(stderr.contains("<TEXT>..."), "{stderr}");
    assert!(stderr.contains("Usage: tui-do add"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "a prompt was drawn: {:?}",
        output.stdout
    );
    assert!(!dir.join("tui-do.db").exists(), "the store was opened");
    let _ = std::fs::remove_dir_all(&dir);
}
