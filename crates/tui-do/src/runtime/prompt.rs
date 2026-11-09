//! `tui-do add` with no text: ask for the task on a few lines of the terminal.
//!
//! Not the interface. The prompt holds [`HEIGHT`] lines at the bottom of the normal
//! screen in raw mode (`TerminalGuard::inline`), with no alternate screen, so the shell's
//! scrollback is still there above it and the line that was added stays there after.
//! What it asks for is handed to [`super::add`] as if it had been typed on the command
//! line, so the two forms cannot drift apart.
//!
//! The line is `tui_do_ui::assist::QuickAddInput`, the same one the interface's `a`
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
use tui_do_core::quickadd::{self, Parsed};
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

/// How the prompt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Answer {
    /// Enter, on a line with a task in it.
    Add(String),
    /// Esc or Ctrl-C.
    Cancelled,
}

/// The prompt's state: the line, and the example shown until the first key.
pub(super) struct Prompt {
    line: QuickAddInput,
    /// Shown dim in the empty line until anything is pressed.
    placeholder: Option<String>,
    now: ZonedTime,
    theme: Theme,
}

impl Prompt {
    /// A prompt that completes from `vocabulary`, its example built from the same names.
    pub(super) fn new(vocabulary: Vocabulary, now: ZonedTime, theme: Theme) -> Self {
        Self {
            placeholder: Some(placeholder(&vocabulary, &now)),
            line: QuickAddInput::new(vocabulary),
            now,
            theme,
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

/// What the line will create, re-read on every key.
// TODO(item 12 step 1): call Parsed::summary
fn preview(parsed: &Parsed, now: &ZonedTime) -> String {
    let mut parts = vec![format!("\"{}\"", super::printable(&parsed.title))];
    if let Some(project) = &parsed.project {
        parts.push(format!("+{}", super::printable(project)));
    }
    parts.extend(
        parsed
            .labels
            .iter()
            .map(|label| format!("*{}", super::printable(label))),
    );
    parts.extend(
        parsed
            .assignees
            .iter()
            .map(|user| format!("@{}", super::printable(user))),
    );
    if let Some(priority) = parsed.priority {
        parts.push(format!("!{priority}"));
    }
    if parsed.repeat.is_some() {
        parts.push("repeats".to_string());
    }
    format!(
        "{}{}",
        parts.join(" "),
        super::dates_suffix(parsed.due_date, parsed.start_date, &now.timezone())
    )
}

/// Ask for a task until Enter, Esc or Ctrl-C.
///
/// `events` is `super::next_event` everywhere but the tests, which feed keys in. The
/// viewport is cleared on the way out, with the cursor left at its top, so whatever
/// the caller prints next starts where the prompt was rather than under nine blank
/// lines.
///
/// # Errors
/// A terminal that cannot be drawn on or read from.
pub(super) fn ask<B: Backend>(
    terminal: &mut Terminal<B>,
    prompt: &mut Prompt,
    mut events: impl FnMut(Duration) -> io::Result<Option<Event>>,
) -> anyhow::Result<Answer>
where
    B::Error: Send + Sync + 'static,
{
    // Drawn when something changed, not on every poll: an idle prompt that repainted ten
    // times a second would fill a slow link and anything recording the session.
    let mut stale = true;
    let answer = loop {
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
            break Answer::Add(prompt.line.value().to_string());
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

    let shown = match &prompt.placeholder {
        Some(example) if value.is_empty() => {
            Span::styled(example.clone(), theme.muted().add_modifier(Modifier::DIM))
        }
        _ => Span::styled(value.to_string(), theme.text()),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(PROMPT, theme.accent()),
            shown,
        ])),
        row(0),
    );
    let prefix = tui_do_ui::rows::display_width(PROMPT);
    let x = area.x + prefix + prompt.line.input.caret_column();
    if x < area.right() {
        frame.set_cursor_position((x, area.y));
    }

    if area.height < 2 {
        return;
    }
    let preview = if value.trim().is_empty() {
        Span::styled(
            "type a task -- Tab completes, ? after + * @ ! or due lists, F1 help, Esc cancels",
            theme.muted(),
        )
    } else {
        let parsed = quickadd::parse(value, &prompt.now);
        Span::styled(
            format!("→ {}", preview(&parsed, &prompt.now)),
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
            projects: vec!["Home Improvement".into(), "Legal".into()],
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
        let mut prompt = Prompt::new(vocabulary(), now(), Theme::new(ColorDepth::None));
        let mut queue: VecDeque<Event> = events.into();
        let answer = ask(&mut terminal, &mut prompt, |_| {
            queue
                .pop_front()
                .map(Some)
                .ok_or_else(|| io::Error::other("no more keys"))
        });
        (answer, terminal)
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
            shown.contains("→ \"Pay rent\" +Legal !3, due Mon 9 Nov 2026"),
            "{shown}"
        );
    }

    #[test]
    fn help_and_matches_draw_under_the_preview() {
        let (_, terminal) = drive(typing("Fix the door +?"));
        let shown = screen(&terminal);
        assert!(shown.contains("add› Fix the door +"), "{shown}");
        assert!(!shown.contains("door +?"), "the ? was typed: {shown}");
        assert!(shown.contains("+[Home Improvement]  +Legal"), "{shown}");
    }
}
