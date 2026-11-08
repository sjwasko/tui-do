//! Owning the terminal, and giving it back.
//!
//! Raw mode and the alternate screen are process-global state. If tui-do exits without
//! undoing them — through an error path, a signal, or a panic in a dependency — the
//! user's shell is left without echo and without a cursor, and their only recourse is
//! `reset`. So the restore happens in a `Drop` *and* in a panic hook, and both are
//! idempotent.
//!
//! The hook runs on whichever thread panicked, and a panic inside a spawned task does not
//! end the process: tokio catches it at the task boundary. So the hook can hand the
//! terminal back while the interface is still running, and from then on nothing may draw
//! on it -- [`TerminalGuard::draw`] checks, and the runtime ends the session through its
//! normal exit path (BUG-7).

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::{Frame, Terminal};

/// Whether the terminal is currently ours. Read by the panic hook, which cannot borrow
/// the guard.
static RAW: AtomicBool = AtomicBool::new(false);

/// The terminal, restored when this is dropped.
///
/// Generic over the backend only so a test can hold one over ratatui's `TestBackend`;
/// the program itself only ever takes the real one.
#[derive(Debug)]
pub struct TerminalGuard<B: Backend = CrosstermBackend<Stdout>> {
    terminal: Terminal<B>,
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
        // A blinking block, not the terminal's default. tui-do shows a cursor only where
        // text is being typed, and in the edit form the focused field is already
        // underlined -- an underline caret would be camouflaged by it, and a thin bar was
        // what the form had in effect before, which is to say nothing findable.
        execute!(
            out,
            EnterAlternateScreen,
            cursor::Hide,
            cursor::SetCursorStyle::BlinkingBlock
        )
        .context("could not switch to the alternate screen")?;

        let terminal = Terminal::new(CrosstermBackend::new(out))
            .context("could not initialise the terminal backend")?;
        Ok(Self { terminal })
    }

    /// The size of the terminal, if it will say.
    pub fn size(&self) -> Option<(u16, u16)> {
        self.terminal.size().ok().map(|s| (s.width, s.height))
    }
}

impl<B: Backend> TerminalGuard<B> {
    /// A guard over any backend, marked as holding the terminal. For tests, which have
    /// no terminal to take.
    #[cfg(test)]
    pub fn over(backend: B) -> anyhow::Result<Self> {
        let terminal =
            Terminal::new(backend).map_err(|error| anyhow::anyhow!("test backend: {error}"))?;
        RAW.store(true, Ordering::SeqCst);
        Ok(Self { terminal })
    }

    /// Draw a frame, unless the terminal has already been given back.
    ///
    /// Returns whether anything was drawn. After the panic hook has restored the
    /// terminal, the user's shell is on screen again, and a frame drawn now lands on top
    /// of it -- the garbled screen BUG-7 was about. So once the terminal is no longer
    /// ours, this draws nothing and waits for the runtime to finish shutting down.
    ///
    /// # Errors
    /// Whatever the backend reports while drawing.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> anyhow::Result<bool> {
        if !held() {
            return Ok(false);
        }
        self.terminal
            .draw(render)
            .map_err(|error| anyhow::anyhow!("could not draw to the terminal: {error}"))?;
        Ok(true)
    }
}

impl<B: Backend> Drop for TerminalGuard<B> {
    fn drop(&mut self) {
        restore();
    }
}

/// Whether the terminal is still ours to draw on.
fn held() -> bool {
    RAW.load(Ordering::SeqCst)
}

/// Undo everything [`TerminalGuard::take`] did. Safe to call twice.
fn restore() {
    restore_to(&mut io::stdout());
}

/// [`restore`], writing its escape sequences to `out`. Returns whether it did anything:
/// only the first call after [`TerminalGuard::take`] does.
fn restore_to(out: &mut impl Write) -> bool {
    if !RAW.swap(false, Ordering::SeqCst) {
        return false;
    }
    // Nothing useful can be done about a failure here: we are already on the way out, and
    // the alternative is a panic inside a panic hook.
    let _ = execute!(
        out,
        cursor::SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = disable_raw_mode();
    let _ = out.flush();
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;

    use super::*;

    /// The panic hook fires while the interface is still running, because tokio caught
    /// the panic and the process carried on. The restore it does must be the only one,
    /// and the guard must stop drawing from that moment -- a frame drawn after it lands
    /// on the user's shell. This is the only test that touches `RAW`, so it does not race
    /// another for the global.
    #[test]
    fn after_the_hook_restores_the_terminal_nothing_draws_or_restores_again() {
        let mut guard = TerminalGuard::over(TestBackend::new(20, 3)).unwrap();
        let hello =
            |frame: &mut Frame<'_>| frame.render_widget(Paragraph::new("hello"), frame.area());
        assert!(guard.draw(hello).unwrap(), "a held terminal is drawn on");

        // What the panic hook does.
        let mut first = Vec::new();
        assert!(restore_to(&mut first), "the first restore does the work");
        assert!(!first.is_empty(), "the first restore writes the escapes");

        // Idempotent: the guard's own `Drop`, or a second panic, does nothing more.
        let mut second = Vec::new();
        assert!(!restore_to(&mut second));
        assert!(second.is_empty(), "a second restore wrote {second:?}");

        // And the event loop's next frame is not drawn over the user's shell.
        assert!(
            !guard.draw(hello).unwrap(),
            "drew into a terminal the panic hook had already given back"
        );
        // Dropping the guard now is the normal shutdown path; with `RAW` spent it is a
        // no-op rather than a second set of escapes on stdout.
        drop(guard);
        assert!(!held());
    }
}
