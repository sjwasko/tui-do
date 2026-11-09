//! SIGTERM, SIGHUP and SIGQUIT, turned into a way out that gives the terminal back (BUG-31).
//!
//! Their default action ends the process without unwinding, so neither the terminal
//! guard's `Drop` nor the panic hook runs and the user's shell comes back with no echo and
//! no line editing. Ctrl-C and Ctrl-\ never get here -- raw mode turns them into bytes --
//! but closing the window, `kill`, a systemd stop or a dropped SSH connection all do.
//!
//! So whoever holds the terminal listens for them, and each one arrives as a [`Signal`]
//! on a channel: the interface's event loop reads it beside its messages and leaves the
//! way `q` does, and the inline `tui-do add` prompt checks it between keys. Once the
//! handlers are installed the default action is gone for good -- tokio never puts it
//! back -- so the listener keeps the old behaviour for whoever is not listening any more:
//! a signal nobody can receive ends the process at once, with the same exit code the
//! default action would have given.
//!
//! `tui-do add --edit` listens for nothing while the editor runs, on purpose. tui-do
//! holds no terminal state then -- raw mode is never on around the editor, which owns the
//! terminal -- and the editor shares tui-do's process group, so a hangup reaches it
//! directly and it restores its own modes. A signal there ends tui-do as it always did
//! (or through the exit above, when the inline prompt came first): nothing is queued
//! yet, and all that is lost is the removal of the editor's temporary file.

use std::future::Future;
use std::io;

use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::UnboundedSender;

/// A signal that ends tui-do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Signal {
    /// SIGTERM: `kill`, a systemd stop.
    Terminate,
    /// SIGHUP: the terminal went away -- a closed window, a dropped SSH connection.
    Hangup,
    /// SIGQUIT, from `kill -QUIT`. Not Ctrl-\, which raw mode reads as a key.
    Quit,
}

impl Signal {
    /// The signal's number.
    const fn number(self) -> i32 {
        match self {
            Self::Hangup => 1,
            Self::Quit => 3,
            Self::Terminate => 15,
        }
    }

    /// What tui-do exits with: `128 + signal`, which is what a shell reports for a
    /// process the signal killed, so a script cannot tell the difference.
    pub(super) const fn exit_code(self) -> i32 {
        128 + self.number()
    }

    /// Whether the terminal is probably gone. Writing to it may fail, or wait on a peer
    /// that will never read, so nothing should depend on reaching it.
    pub(super) const fn hung_up(self) -> bool {
        matches!(self, Self::Hangup)
    }
}

/// Install the handlers, and answer the task that forwards each signal to `to`.
///
/// Called inside a tokio runtime, which the handlers belong to; the caller spawns what
/// comes back. Once `to`'s receiver has gone, a signal ends the process on the spot with
/// its exit code, as it would have without a handler: tui-do no longer holds the
/// terminal then, so there is nothing left to give back.
///
/// That exit is the third way BUG-31's fix can abandon a push mid-request (the other
/// two are in `super::flush_unless_signalled`). After the inline prompt has returned,
/// `tui-do add` goes on to push the task it queued, and a signal during that request
/// ends the process here, with the request maybe already at the server. It is the BUG-3
/// class -- the entry stays queued and the next launch may create the task a second
/// time (BUG-33) -- and it is exactly what the default action did before: `add` given
/// its text on the command line installs no handler and is still ended that way.
///
/// `None`, with a warning in the log, if the operating system refuses a handler. tui-do
/// carries on without one: the signals then end it as they always did.
pub(super) fn listen(to: UnboundedSender<Signal>) -> Option<impl Future<Output = ()> + Send> {
    match handlers(to) {
        Ok(listener) => Some(listener),
        Err(error) => {
            tracing::warn!(%error, "could not listen for SIGTERM, SIGHUP or SIGQUIT");
            None
        }
    }
}

/// [`listen`], with the refusal as an error.
fn handlers(to: UnboundedSender<Signal>) -> io::Result<impl Future<Output = ()> + Send> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut quit = signal(SignalKind::quit())?;
    Ok(async move {
        loop {
            let caught = tokio::select! {
                Some(()) = terminate.recv() => Signal::Terminate,
                Some(()) = hangup.recv() => Signal::Hangup,
                Some(()) = quit.recv() => Signal::Quit,
                else => return,
            };
            if to.send(caught).is_err() {
                std::process::exit(caught.exit_code());
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_signal_exits_as_a_shell_reports_it() {
        assert_eq!(Signal::Terminate.exit_code(), 143);
        assert_eq!(Signal::Hangup.exit_code(), 129);
        assert_eq!(Signal::Quit.exit_code(), 131);
        assert!(Signal::Hangup.hung_up());
        assert!(!Signal::Terminate.hung_up() && !Signal::Quit.hung_up());
    }
}
