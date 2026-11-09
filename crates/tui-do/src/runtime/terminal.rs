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
//!
//! Two ways to hold it. [`TerminalGuard::take`] is the interface: raw mode and the
//! alternate screen. [`TerminalGuard::inline`] is `tui-do add` asking for a task: raw mode
//! and a few lines at the bottom of the normal screen, so what was typed stays in the
//! scrollback. The restore undoes only what was done, because leaving an alternate screen
//! that was never entered restores a cursor position nobody saved.

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

/// Whether the terminal is currently ours. Read by the panic hook, which cannot borrow
/// the guard.
static RAW: AtomicBool = AtomicBool::new(false);

/// Whether the alternate screen was entered, so the restore knows to leave it.
static ALTERNATE: AtomicBool = AtomicBool::new(false);

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
        install_hook();
        enable_raw_mode().context("could not put the terminal into raw mode")?;
        RAW.store(true, Ordering::SeqCst);
        ALTERNATE.store(true, Ordering::SeqCst);
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

    /// Take over `height` lines at the bottom of the normal screen, in raw mode.
    ///
    /// No alternate screen, so whatever is left in those lines when the guard goes is
    /// still there afterwards. The panic hook and the `Drop` are the same ones
    /// [`Self::take`] installs.
    ///
    /// # Errors
    /// Whatever the terminal reports when raw mode is refused, or when it will not say
    /// where its cursor is.
    pub fn inline(height: u16) -> anyhow::Result<Self> {
        install_hook();
        enable_raw_mode().context("could not put the terminal into raw mode")?;
        RAW.store(true, Ordering::SeqCst);
        let terminal = Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )
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

    /// The terminal itself, for a caller that draws from one thread with nothing
    /// spawned that could panic under it -- the inline prompt, which has no tasks.
    pub fn terminal(&mut self) -> &mut Terminal<B> {
        &mut self.terminal
    }

    /// The backend, for a test to read what was drawn.
    #[cfg(test)]
    pub fn backend(&self) -> &B {
        self.terminal.backend()
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

/// Have a panic on any thread give the terminal back before it is reported.
fn install_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
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
pub(super) fn restore_to(out: &mut impl Write) -> bool {
    if !RAW.swap(false, Ordering::SeqCst) {
        return false;
    }
    // Nothing useful can be done about a failure here: we are already on the way out, and
    // the alternative is a panic inside a panic hook.
    if ALTERNATE.swap(false, Ordering::SeqCst) {
        let _ = execute!(
            out,
            cursor::SetCursorStyle::DefaultUserShape,
            LeaveAlternateScreen
        );
    }
    let _ = execute!(out, cursor::Show);
    let _ = disable_raw_mode();
    let _ = out.flush();
    true
}

/// Serialise the tests that hold a guard. `RAW` is process-global, and two tests taking
/// and releasing it in parallel would each see the other's changes.
#[cfg(test)]
pub(super) fn lock_for_test() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    /// on the user's shell.
    #[test]
    fn after_the_hook_restores_the_terminal_nothing_draws_or_restores_again() {
        let _terminal = lock_for_test();
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

    /// The inline prompt never entered the alternate screen, and leaving one that was
    /// not entered restores a cursor position nobody saved, so the prompt's output
    /// would jump. Its restore gives back raw mode and the cursor, and nothing else.
    #[test]
    fn an_inline_restore_does_not_leave_an_alternate_screen() {
        let _terminal = lock_for_test();
        let guard = TerminalGuard::over(TestBackend::new(20, 3)).unwrap();
        let mut inline = Vec::new();
        assert!(restore_to(&mut inline));
        let inline = String::from_utf8_lossy(&inline).to_string();
        assert!(
            !inline.contains("\x1b[?1049l"),
            "left an alternate screen: {inline:?}"
        );
        assert!(
            inline.contains("\x1b[?25h"),
            "the cursor was not shown: {inline:?}"
        );

        // And the interface's restore still leaves it.
        RAW.store(true, Ordering::SeqCst);
        ALTERNATE.store(true, Ordering::SeqCst);
        let mut full = Vec::new();
        assert!(restore_to(&mut full));
        let full = String::from_utf8_lossy(&full).to_string();
        assert!(full.contains("\x1b[?1049l"), "{full:?}");
        drop(guard);
    }
}
