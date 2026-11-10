//! `tui-do add` with no text: ask for the task on a few lines of the terminal.
//!
//! Not the interface. The prompt holds [`HEIGHT`] lines at the bottom of the normal
//! screen in raw mode (`TerminalGuard::inline`), with no alternate screen, so the shell's
//! scrollback is still there above it and the line that was added stays there after.
//! What it asks for is handed to [`super::add`] as if it had been typed on the command
//! line, so the two forms cannot drift apart.
//!
//! The line is `tui_do_ui::assist::QuickAddInput`, the same one the interface's `n`
//! prompt holds, so Tab and `?` mean the same thing in both.

use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode};
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use tui_do_core::quickadd;
use tui_do_core::ZonedTime;
use tui_do_ui::assist::{QuickAddInput, Vocabulary};
use tui_do_ui::theme::Theme;
use tui_do_ui::Key;

/// How many rows the matches or help may take under the preview.
const ASIDE_ROWS: u16 = 7;

/// The lines the prompt holds: the input, the preview, and room for an aside.
pub(super) const HEIGHT: u16 = 2 + ASIDE_ROWS;

/// What goes in front of what is typed.
const PROMPT: &str = "add› ";

/// The same, for `tui-do add --dry-run` with no text: the line will be checked, not added.
const DRY_RUN_PROMPT: &str = "dry run› ";

/// How the prompt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Answer {
    /// Enter, on a line with a task in it.
    Add(String),
    /// Esc or Ctrl-C.
    Cancelled,
    /// SIGTERM, SIGHUP or SIGQUIT arrived while the prompt held the terminal (BUG-31).
    Signalled(super::Signal),
}

/// The prompt's state: the line, and the example shown until the first key.
pub(super) struct Prompt {
    line: QuickAddInput,
    /// Shown dim in the empty line until anything is pressed.
    placeholder: Option<String>,
    now: ZonedTime,
    theme: Theme,
    dry_run: bool,
}

impl Prompt {
    /// A prompt that completes from `vocabulary`, its example built from the same names.
    pub(super) fn new(vocabulary: Vocabulary, now: ZonedTime, theme: Theme, dry_run: bool) -> Self {
        Self {
            placeholder: Some(placeholder(&vocabulary, &now)),
            line: QuickAddInput::new(vocabulary),
            now,
            theme,
            dry_run,
        }
    }

    /// What goes in front of the line, saying whether Enter adds it or only checks it.
    pub(super) const fn prefix(&self) -> &'static str {
        if self.dry_run {
            DRY_RUN_PROMPT
        } else {
            PROMPT
        }
    }
}

/// An example of the recommended shape, in the user's own project and label names.
///
/// Their first project and label where the store has any, so the example is a line they
/// could actually send; the static names otherwise. The date is four days out, written
/// in full after `due`, which is the one form no sentence can misread.
pub(super) fn placeholder(vocabulary: &Vocabulary, now: &ZonedTime) -> String {
    let token = |sigil: char, name: Option<&String>, fallback: &str| {
        let name = name.map_or(fallback, String::as_str);
        if name.contains(char::is_whitespace) {
            format!("{sigil}[{name}]")
        } else {
            format!("{sigil}{name}")
        }
    };
    let day = (now.date_naive() + chrono::Days::new(4)).format("%Y-%m-%d");
    format!(
        "Call the dentist {} {} !3 due {day}",
        token('+', vocabulary.projects.first(), "Personal"),
        token('*', vocabulary.labels.first(), "health"),
    )
}

/// Ask for a task until Enter, Esc, Ctrl-C or a signal.
///
/// `events` is `super::next_event` everywhere but the tests, which feed keys in.
/// `signalled` answers a signal that has arrived, if one has; it is asked before every
/// poll, so one is noticed within [`super::INPUT_POLL`]. The viewport is cleared on the
/// way out, with the cursor left at its top, so whatever the caller prints next starts
/// where the prompt was rather than under nine blank lines -- except after SIGHUP, when
/// the terminal has probably gone and a write to it could fail or wait.
///
/// # Errors
/// A terminal that cannot be drawn on or read from.
pub(super) fn ask<B: Backend>(
    terminal: &mut Terminal<B>,
    prompt: &mut Prompt,
    mut events: impl FnMut(Duration) -> io::Result<Option<Event>>,
    mut signalled: impl FnMut() -> Option<super::Signal>,
) -> anyhow::Result<Answer>
where
    B::Error: Send + Sync + 'static,
{
    // Drawn when something changed, not on every poll: an idle prompt that repainted ten
    // times a second would fill a slow link and anything recording the session.
    let mut stale = true;
    let answer = loop {
        if let Some(caught) = signalled() {
            if caught.hung_up() {
                return Ok(Answer::Signalled(caught));
            }
            break Answer::Signalled(caught);
        }
        if stale {
            terminal.draw(|frame| render(frame, prompt))?;
            stale = false;
        }
        let event = match events(super::INPUT_POLL)? {
            Some(Event::Key(event)) => event,
            Some(Event::Resize(..)) => {
                stale = true;
                continue;
            }
            _ => continue,
        };
        stale = true;
        let Some(key) = Key::from_event(event) else {
            continue;
        };
        prompt.placeholder = None;
        if key == Key::ctrl('c') || key.code == KeyCode::Esc {
            break Answer::Cancelled;
        }
        if key.code == KeyCode::Enter {
            // An empty line is not a task; stay, rather than leave on a stray Enter.
            if prompt.line.value().trim().is_empty() {
                continue;
            }
            // `None` while a bracket is open; the line says why.
            match prompt.line.submit() {
                Some(line) => break Answer::Add(line),
                None => continue,
            }
        }
        prompt.line.press(key);
    };
    terminal.clear()?;
    let top = terminal.get_frame().area().as_position();
    terminal.set_cursor_position(top)?;
    Ok(answer)
}

/// Draw the line, the preview under it, and any matches or help under that.
fn render(frame: &mut Frame<'_>, prompt: &Prompt) {
    let area = frame.area();
    let theme = prompt.theme;
    let row = |y: u16| Rect {
        x: area.x,
        y: area.y + y,
        width: area.width,
        height: 1,
    };
    let value = prompt.line.value();
    // Windowed to the caret, so a line longer than the terminal scrolls (BUG-32).
    let prefix = tui_do_ui::rows::display_width(prompt.prefix());
    let field = prompt.line.input.window(area.width.saturating_sub(prefix));

    let shown = match &prompt.placeholder {
        Some(example) if value.is_empty() => {
            Span::styled(example.clone(), theme.muted().add_modifier(Modifier::DIM))
        }
        _ => Span::styled(field.text, theme.text()),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prompt.prefix(), theme.accent()),
            shown,
        ])),
        row(0),
    );
    let x = area.x + prefix + field.caret_x;
    if x < area.right() {
        frame.set_cursor_position((x, area.y));
    }

    if area.height < 2 {
        return;
    }
    let preview = if let Some(open) = tui_do_ui::assist::unclosed(value) {
        Span::styled(format!("! {}", open.hint()), theme.warning())
    } else if value.trim().is_empty() {
        Span::styled(
            "type a task -- Tab completes, ? after + * @ ! or due lists, F1 help, Esc cancels",
            theme.muted(),
        )
    } else {
        let parsed = quickadd::parse(value, &prompt.now);
        Span::styled(
            // The summary a dry run prints, so the preview and `--dry-run` agree word for
            // word. Control characters out, as everywhere tui-do prints a title.
            format!(
                "→ {}",
                super::printable(&parsed.summary(&prompt.now.timezone()))
            ),
            theme.accent(),
        )
    };
    frame.render_widget(Paragraph::new(Line::from(preview)), row(1));

    if let Some(aside) = prompt.line.aside() {
        let rows = usize::from(area.height.saturating_sub(2));
        let lines: Vec<Line> = aside
            .lines(area.width, rows)
            .into_iter()
            .enumerate()
            .map(|(at, line)| {
                let style = if at == 0 {
                    theme.accent()
                } else {
                    theme.text()
                };
                Line::from(Span::styled(line, style))
            })
            .collect();
        let below = Rect {
            y: area.y + 2,
            height: area.height.saturating_sub(2),
            ..area
        };
        frame.render_widget(Paragraph::new(lines), below);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::VecDeque;

    use chrono::TimeZone as _;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::{TerminalOptions, Viewport};
    use tui_do_ui::theme::ColorDepth;

    use super::*;

    fn now() -> ZonedTime {
        tui_do_core::zone::pinned(
            chrono::FixedOffset::west_opt(5 * 3600)
                .unwrap()
                .with_ymd_and_hms(2026, 11, 8, 10, 0, 0)
                .unwrap(),
        )
    }

    fn vocabulary() -> Vocabulary {
        Vocabulary {
            projects: vec![
                "Home Improvement".into(),
                "Home Office".into(),
                "Legal".into(),
            ],
            labels: vec!["urgent".into()],
            users: vec!["admin".into()],
        }
    }

    fn terminal() -> Terminal<TestBackend> {
        Terminal::with_options(
            TestBackend::new(70, 14),
            TerminalOptions {
                viewport: Viewport::Inline(HEIGHT),
            },
        )
        .unwrap()
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typing(text: &str) -> Vec<Event> {
        text.chars().map(|c| key(KeyCode::Char(c))).collect()
    }

    /// Feed `events` to the prompt, one per poll, and draw what is on screen when the
    /// keys run out -- the prompt never sees the end, it sees an error.
    fn drive(events: Vec<Event>) -> (anyhow::Result<Answer>, Terminal<TestBackend>) {
        let mut terminal = terminal();
        let mut prompt = Prompt::new(vocabulary(), now(), Theme::new(ColorDepth::None), false);
        let answer = resume(&mut terminal, &mut prompt, events);
        (answer, terminal)
    }

    /// Carry on with a prompt the keys ran out on, as if the user had paused.
    fn resume(
        terminal: &mut Terminal<TestBackend>,
        prompt: &mut Prompt,
        events: Vec<Event>,
    ) -> anyhow::Result<Answer> {
        let mut queue: VecDeque<Event> = events.into();
        ask(
            terminal,
            prompt,
            |_| {
                queue
                    .pop_front()
                    .map(Some)
                    .ok_or_else(|| io::Error::other("no more keys"))
            },
            || None,
        )
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn typing_then_enter_answers_the_line_as_the_one_shot_form_would_get_it() {
        let typed = "Call the VA +Le";
        let mut events = typing(typed);
        events.push(key(KeyCode::Tab));
        events.extend(typing("due tomorrow"));
        events.push(key(KeyCode::Enter));
        let (answer, _) = drive(events);
        let line = "Call the VA +Legal due tomorrow";
        assert_eq!(answer.unwrap(), Answer::Add(line.to_string()));
        // `run_add` hands this to `runtime::add` exactly as it does `add 'text'`, so
        // what is parsed is what the one-shot form parses.
        let parsed = quickadd::parse(line, &now());
        assert_eq!(parsed.project.as_deref(), Some("Legal"));
        assert_eq!(parsed.title, "Call the VA");
        assert!(parsed.due_date.is_some());
    }

    #[test]
    fn enter_waits_while_a_bracket_is_open() {
        // Tab completes as far as the two `Home` projects agree, leaving `+[Home ` open;
        // an Enter then would have filed it under `[Home` with `Office` in the title.
        let mut events = typing("Fix the door +Ho");
        events.push(key(KeyCode::Tab));
        events.push(key(KeyCode::Enter));
        let (answer, terminal) = drive(events);
        assert!(answer.is_err(), "nothing was answered: {answer:?}");
        let shown = screen(&terminal);
        assert!(
            shown.contains("! +[Home is not closed"),
            "the preview: {shown}"
        );

        let mut events = typing("Fix the door +Ho");
        events.push(key(KeyCode::Tab));
        events.extend(typing("I"));
        events.push(key(KeyCode::Tab));
        events.push(key(KeyCode::Enter));
        assert_eq!(
            drive(events).0.unwrap(),
            Answer::Add("Fix the door +[Home Improvement] ".to_string())
        );
    }

    #[test]
    fn a_line_longer_than_the_terminal_scrolls_to_the_caret() {
        // BUG-32: the whole line was drawn and the caret dropped once it passed the edge.
        let long = format!("BEGIN{}FINAL", "x".repeat(90));
        let mut terminal = terminal();
        let mut prompt = Prompt::new(vocabulary(), now(), Theme::new(ColorDepth::None), false);
        let _ = resume(&mut terminal, &mut prompt, typing(&long));
        let drawn = screen(&terminal);
        let (y, row) = drawn
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("FINAL"))
            .unwrap_or_else(|| panic!("the end is not visible:\n{drawn}"));
        assert!(!row.contains("BEGIN"), "{row}");
        let after = row[..row.find("FINAL").unwrap()].chars().count() + 5;
        let caret = terminal.backend().cursor_position();
        assert!(terminal.backend().cursor_visible());
        assert_eq!((usize::from(caret.x), usize::from(caret.y)), (after, y));

        let _ = resume(&mut terminal, &mut prompt, vec![key(KeyCode::Home)]);
        let drawn = screen(&terminal);
        let row = drawn.lines().nth(y).unwrap();
        assert!(row.contains("BEGIN") && !row.contains("FINAL"), "{row}");
    }

    #[test]
    fn esc_and_ctrl_c_cancel() {
        let mut events = typing("Call the VA");
        events.push(key(KeyCode::Esc));
        assert_eq!(drive(events).0.unwrap(), Answer::Cancelled);

        let mut events = typing("Call the VA");
        events.push(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(drive(events).0.unwrap(), Answer::Cancelled);
    }

    /// BUG-31: a signal is noticed between polls, before the next key is read, and ends
    /// the prompt with the signal. Cleared away like any other answer -- except after
    /// SIGHUP, when the terminal has probably gone and nothing more is written to it.
    #[test]
    fn a_signal_ends_the_prompt_between_keys() {
        for (signal, cleared) in [
            (super::super::Signal::Terminate, true),
            (super::super::Signal::Hangup, false),
        ] {
            let mut terminal = terminal();
            let mut prompt = Prompt::new(vocabulary(), now(), Theme::new(ColorDepth::None), false);
            let polled = std::cell::Cell::new(0);
            let answer = ask(
                &mut terminal,
                &mut prompt,
                |_| {
                    polled.set(polled.get() + 1);
                    Ok(Some(key(KeyCode::Char('x'))))
                },
                || (polled.get() == 3).then_some(signal),
            );
            assert_eq!(answer.unwrap(), Answer::Signalled(signal));
            assert_eq!(polled.get(), 3, "read a key after the signal");
            assert_eq!(
                prompts_on(&terminal),
                usize::from(!cleared),
                "{signal:?}: {}",
                screen(&terminal)
            );
        }
    }

    #[test]
    fn enter_on_an_empty_line_adds_nothing_and_stays() {
        let (answer, _) = drive(vec![key(KeyCode::Enter), key(KeyCode::Esc)]);
        assert_eq!(answer.unwrap(), Answer::Cancelled);
    }

    #[test]
    fn the_placeholder_is_built_from_real_names_and_vanishes_on_the_first_key() {
        let example = placeholder(&vocabulary(), &now());
        assert_eq!(
            example,
            "Call the dentist +[Home Improvement] *urgent !3 due 2026-11-12"
        );
        assert_eq!(
            placeholder(&Vocabulary::default(), &now()),
            "Call the dentist +Personal *health !3 due 2026-11-12",
            "no store, the static example"
        );

        // Before any key: the example is drawn.
        let (_, terminal) = drive(vec![]);
        assert!(
            screen(&terminal).contains(&example),
            "{}",
            screen(&terminal)
        );

        // One key, then the keys run out mid-prompt.
        let (_, terminal) = drive(typing("x"));
        let shown = screen(&terminal);
        assert!(!shown.contains("dentist"), "{shown}");
        assert!(shown.contains("add› x"), "{shown}");

        // And it does not come back when the line is emptied again.
        let mut events = typing("x");
        events.push(key(KeyCode::Backspace));
        let (_, terminal) = drive(events);
        assert!(!screen(&terminal).contains("dentist"));
    }

    #[test]
    fn the_preview_reparses_on_every_key() {
        let (_, terminal) = drive(typing("Pay rent +Legal !3 due tomorrow"));
        let shown = screen(&terminal);
        assert!(
            shown.contains("→ title \"Pay rent\" · +Legal · !3 · due Mon 9 Nov 2026"),
            "{shown}"
        );
    }

    #[test]
    fn help_and_matches_draw_under_the_preview() {
        let (_, terminal) = drive(typing("Fix the door +?"));
        let shown = screen(&terminal);
        assert!(shown.contains("add› Fix the door +"), "{shown}");
        assert!(!shown.contains("door +?"), "the ? was typed: {shown}");
        assert!(
            shown.contains("+[Home Improvement]  +[Home Office]  +Legal"),
            "{shown}"
        );
    }

    /// How many rows of the screen hold the prompt line.
    fn prompts_on(terminal: &Terminal<TestBackend>) -> usize {
        screen(terminal)
            .lines()
            .filter(|row| row.contains("add›"))
            .count()
    }

    #[test]
    fn a_resize_redraws_in_place_and_leaves_nothing_behind() {
        let mut terminal = terminal();
        let mut prompt = Prompt::new(vocabulary(), now(), Theme::new(ColorDepth::None), false);
        let line = "Pay rent +Legal !3 due tomorrow";
        let mut events = typing(line);
        events.push(key(KeyCode::F(1)));
        assert!(resume(&mut terminal, &mut prompt, events).is_err());
        assert_eq!(prompts_on(&terminal), 1);

        // Narrower, then wider, each announced the way crossterm does.
        for (width, height) in [(40, 14), (90, 20)] {
            terminal.backend_mut().resize(width, height);
            let resized = vec![Event::Resize(width, height)];
            assert!(resume(&mut terminal, &mut prompt, resized).is_err());
            let shown = screen(&terminal);
            assert_eq!(
                prompts_on(&terminal),
                1,
                "a stale prompt at {width}:\n{shown}"
            );
            assert_eq!(
                shown.matches("Anywhere in the line:").count(),
                1,
                "the help is drawn once at {width}:\n{shown}"
            );
            assert!(shown.contains("add› Pay rent"), "{shown}");
        }

        // The line survives, and the way out still clears the prompt away.
        let answer = resume(&mut terminal, &mut prompt, vec![key(KeyCode::Enter)]);
        assert_eq!(answer.unwrap(), Answer::Add(line.to_string()));
        assert_eq!(prompts_on(&terminal), 0, "{}", screen(&terminal));
    }

    #[test]
    fn a_dry_run_prompt_says_so_and_answers_the_line_all_the_same() {
        let mut terminal = terminal();
        let mut prompt = Prompt::new(vocabulary(), now(), Theme::new(ColorDepth::None), true);
        assert!(resume(&mut terminal, &mut prompt, typing("Pay rent")).is_err());
        let shown = screen(&terminal);
        assert!(shown.contains("dry run› Pay rent"), "{shown}");
        assert!(!shown.contains("add›"), "{shown}");
        let answer = resume(&mut terminal, &mut prompt, vec![key(KeyCode::Enter)]);
        assert_eq!(answer.unwrap(), Answer::Add("Pay rent".to_string()));
    }
}
