//! Owning the terminal, and giving it back.
//!
//! Raw mode and the alternate screen are process-global state. If criax exits without
//! undoing them — through an error path, a signal, or a panic in a dependency — the
//! user's shell is left without echo and without a cursor, and their only recourse is
//! `reset`. So the restore happens in a `Drop` *and* in a panic hook, and both are
//! idempotent.

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// Whether the terminal is currently ours. Read by the panic hook, which cannot borrow
/// the guard.
static RAW: AtomicBool = AtomicBool::new(false);

/// The terminal, restored when this is dropped.
#[derive(Debug)]
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Take over the terminal and install the panic hook that gives it back.
    ///
    /// # Errors
    /// Whatever the terminal reports when raw mode or the alternate screen is refused.
    pub fn take() -> anyhow::Result<Self> {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
        }));

        enable_raw_mode().context("could not put the terminal into raw mode")?;
        RAW.store(true, Ordering::SeqCst);
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen, cursor::Hide)
            .context("could not switch to the alternate screen")?;

        let terminal = Terminal::new(CrosstermBackend::new(out))
            .context("could not initialise the terminal backend")?;
        Ok(Self { terminal })
    }

    /// The terminal to draw on.
    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Undo everything [`TerminalGuard::take`] did. Safe to call twice.
fn restore() {
    if !RAW.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    // Nothing useful can be done about a failure here: we are already on the way out, and
    // the alternative is a panic inside a panic hook.
    let _ = execute!(out, LeaveAlternateScreen, cursor::Show);
    let _ = disable_raw_mode();
    let _ = out.flush();
}
