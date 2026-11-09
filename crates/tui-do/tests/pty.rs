//! The built binary on a pseudo-terminal, and the terminal it leaves behind.
//!
//! Each run happens under util-linux `script`, inside a small `sh` script that starts
//! tui-do, waits until it has the terminal in raw mode, does whatever the test asks --
//! nothing, or a signal (BUG-31) -- waits for tui-do to exit within a bound, and then prints the
//! terminal's modes with `stty -a`: the same terminal, read after tui-do has gone. Where
//! there is no `script`, each test says it was skipped and passes.
//!
//! Every run has a scratch config *and* a scratch store, never one: a scratch config alone
//! still writes to the real store. The server is a loopback port nothing listens on, and
//! sync is off.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What a terminal says when asked where its cursor is, as if it were in the top corner.
/// `script` has no emulator behind it, so nothing else answers the inline prompt's query.
const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tui-do-pty-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.yaml"),
        "server:\n  url: http://127.0.0.1:9\n  token_file: token\n\
         sync:\n  interval_seconds: 300\n  enabled: false\n",
    )
    .unwrap();
    std::fs::write(dir.join("token"), "tk_not_a_real_token\n").unwrap();
    std::fs::set_permissions(
        dir.join("token"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    dir
}

/// Quote `text` for `sh`.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn have_script() -> bool {
    Command::new("script")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// What one run left behind.
#[derive(Debug)]
struct Run {
    /// The exit status `sh` saw, or `None` if tui-do outlived its bound and was killed.
    code: Option<i32>,
    /// Whether tui-do had raw mode on at some point. A run that never got there proves
    /// nothing about giving it back.
    was_raw: bool,
    /// `stty -a`'s words, read after tui-do exited.
    modes: Vec<String>,
    /// Everything on the terminal, for the failure message.
    transcript: String,
}

impl Run {
    fn mode(&self, name: &str) -> bool {
        self.modes.iter().any(|word| word == name)
    }

    /// Assert the run exited with `code` and left the terminal cooked.
    fn assert_restored(&self, code: i32) {
        assert!(self.was_raw, "tui-do never took the terminal: {self:#?}");
        assert_eq!(self.code, Some(code), "{self:#?}");
        assert!(self.mode("echo"), "echo was left off: {:?}", self.modes);
        assert!(
            self.mode("icanon"),
            "line editing was left off: {:?}",
            self.modes
        );
    }
}

/// Run tui-do with `args` under a pty. Once it is in raw mode, `then` runs in the shell,
/// with tui-do's pid in `$pid`. With `answer_cursor`, every cursor-position query tui-do
/// writes is answered as a terminal emulator would. `None` if there is no `script` here.
fn on_a_pty(dir: &Path, args: &[&str], answer_cursor: bool, then: &str) -> Option<Run> {
    if !have_script() {
        return None;
    }
    let mut line = sh_quote(env!("CARGO_BIN_EXE_tui-do"));
    for arg in args {
        line.push(' ');
        line.push_str(&sh_quote(arg));
    }
    // `< /dev/tty` because a background job's stdin is otherwise /dev/null. There is no
    // job control in `sh -c`, so tui-do stays in the terminal's foreground group and may
    // set its modes. Raw mode is waited for for 10 s at most, and the exit for 5 s, after
    // which tui-do is killed and the run reports no code.
    let body = format!(
        r#"stty rows 24 cols 80
{line} < /dev/tty > /dev/tty 2>&1 &
pid=$!
n=0
until stty -a | grep -q -- '-icanon'; do
  n=$((n+1)); [ $n -gt 200 ] && break; sleep 0.05
done
if stty -a | grep -q -- '-icanon'; then echo "@raw=1"; else echo "@raw=0"; fi
{then}
n=0
while kill -0 $pid 2>/dev/null; do
  n=$((n+1))
  if [ $n -gt 100 ]; then echo "@hung"; kill -KILL $pid; break; fi
  sleep 0.05
done
wait $pid
echo "@exit=$?"
echo "@modes $(stty -a | tr '\n' ' ')"
"#
    );
    let mut child = Command::new("script")
        .args(["-qec", &body, "/dev/null"])
        .env("TUI_DO_CONFIG", dir.join("config.yaml"))
        .env("TUI_DO_DB", dir.join("tui-do.db"))
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("script runs");
    // Read as it arrives, so each reply goes in while tui-do is waiting for it.
    let mut keyboard = child.stdin.take();
    let mut screen = child.stdout.take().unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut answered = 0;
    loop {
        let read = screen.read(&mut chunk).unwrap_or(0);
        if read == 0 {
            break;
        }
        seen.extend_from_slice(&chunk[..read]);
        let asked = seen.windows(4).filter(|at| at == b"\x1b[6n").count();
        if answer_cursor && asked > answered {
            if let Some(keys) = keyboard.as_mut() {
                for _ in answered..asked {
                    let _ = keys.write_all(CURSOR_REPLY);
                }
                let _ = keys.flush();
            }
            answered = asked;
        }
    }
    drop(keyboard);
    let _ = child.wait();

    let transcript = String::from_utf8_lossy(&seen).into_owned();
    let field = |key: &str| {
        transcript.lines().find_map(|line| {
            line.split_once(key)
                .map(|(_, rest)| rest.trim().to_string())
        })
    };
    Some(Run {
        code: if transcript.contains("@hung") {
            None
        } else {
            field("@exit=").and_then(|code| code.parse().ok())
        },
        was_raw: field("@raw=").as_deref() == Some("1"),
        modes: field("@modes ")
            .unwrap_or_default()
            .split([' ', ';'])
            .map(str::to_string)
            .collect(),
        transcript,
    })
}

const SKIPPED: &str = "skipped: no util-linux `script` to make a terminal with";

/// The inline prompt asks the terminal where its cursor is, and fails if nothing answers.
/// Raw mode was already on by then and no guard existed yet to turn it off, so the error
/// was printed onto a terminal with no echo.
#[test]
fn a_prompt_that_cannot_find_its_cursor_gives_the_terminal_back() {
    let dir = scratch("no-cursor");
    let Some(run) = on_a_pty(&dir, &["add", "--offline"], false, ":") else {
        eprintln!("{SKIPPED}");
        return;
    };
    run.assert_restored(1);
    assert!(
        run.transcript
            .contains("could not initialise the terminal backend"),
        "{run:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// BUG-31: SIGTERM's default action ends the process without unwinding, so neither the
/// guard's `Drop` nor the panic hook ran, and the shell came back raw on the alternate
/// screen. It must leave the way `q` does and exit as a shell reports a SIGTERM.
#[test]
fn sigterm_ends_the_interface_and_gives_the_terminal_back() {
    let dir = scratch("interface-term");
    let Some(run) = on_a_pty(&dir, &[], false, "sleep 0.5; kill -TERM $pid") else {
        eprintln!("{SKIPPED}");
        return;
    };
    run.assert_restored(143);
    assert!(
        run.transcript.contains("\x1b[?1049l"),
        "the alternate screen was not left: {run:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The other two, each with its own code. SIGHUP's restore is not waited on, but a
/// `kill -HUP` leaves the terminal alive, and the restore must still reach it.
#[test]
fn sighup_and_sigquit_end_the_interface_with_their_own_codes() {
    for (signal, code) in [("HUP", 129), ("QUIT", 131)] {
        let dir = scratch(&format!("interface-{signal}"));
        let then = format!("sleep 0.5; kill -{signal} $pid");
        let Some(run) = on_a_pty(&dir, &[], false, &then) else {
            eprintln!("{SKIPPED}");
            return;
        };
        run.assert_restored(code);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The inline prompt takes the terminal too. Signalled with its cursor query answered,
/// so the prompt is up and reading keys when the signal lands, and nothing is added.
#[test]
fn sigterm_ends_the_add_prompt_and_gives_the_terminal_back() {
    let dir = scratch("prompt-term");
    let then = "sleep 1; kill -TERM $pid";
    let Some(run) = on_a_pty(&dir, &["add", "--offline"], true, then) else {
        eprintln!("{SKIPPED}");
        return;
    };
    run.assert_restored(143);
    assert!(
        run.transcript.contains("add›"),
        "the prompt never drew: {run:#?}"
    );
    assert!(
        !dir.join("tui-do.db").exists(),
        "a signalled prompt opened a store: {run:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
