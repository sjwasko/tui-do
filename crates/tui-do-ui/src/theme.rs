//! Colour, as semantic slots rather than literal values.
//!
//! Nothing outside this module names a colour. A row asks for `theme.overdue()`, not for
//! red, which is what makes a second theme a table of values instead of a search through
//! the renderer.
//!
//! Terminal capability arrives from outside rather than being sniffed here: the runtime
//! knows whether it is talking to a truecolor terminal, and `tui-do-ui` stays a pure
//! function of what it is told.

use chrono::{DateTime, FixedOffset, Utc};
use ratatui::style::{Color, Modifier, Style};

/// How much colour the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDepth {
    /// `NO_COLOR`, or a terminal that says it cannot.
    None,
    /// The sixteen ANSI colours.
    Ansi16,
    /// The 256-colour cube.
    Ansi256,
    /// 24-bit colour.
    #[default]
    TrueColor,
}

/// One colour, with the fallbacks for terminals that cannot show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shade {
    rgb: (u8, u8, u8),
    ansi: Color,
}

impl Shade {
    const fn new(rgb: (u8, u8, u8), ansi: Color) -> Self {
        Self { rgb, ansi }
    }

    fn at(self, depth: ColorDepth) -> Color {
        match depth {
            ColorDepth::None => Color::Reset,
            ColorDepth::Ansi16 => self.ansi,
            ColorDepth::Ansi256 => Color::Indexed(index_for(self.rgb)),
            ColorDepth::TrueColor => Color::Rgb(self.rgb.0, self.rgb.1, self.rgb.2),
        }
    }
}

/// The nearest colour in the 256-colour cube.
///
/// The greyscale ramp is checked separately: rounding a near-grey through the 6×6×6 cube
/// lands on a visibly tinted colour, which is exactly where muted text lives.
fn index_for((r, g, b): (u8, u8, u8)) -> u8 {
    /// How far the channels may differ and still read as grey on a terminal.
    const GREY_SPREAD: u32 = 24;

    let spread = u32::from(r.max(g).max(b) - r.min(g).min(b));
    if spread < GREY_SPREAD {
        let level = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
        if level < 8 {
            return 16;
        }
        if level > 248 {
            return 231;
        }
        return 232 + ((level - 8) * 24 / 240).min(23) as u8;
    }
    let axis = |value: u8| -> u32 { (u32::from(value).saturating_sub(35) + 20) / 40 };
    (16 + 36 * axis(r) + 6 * axis(g) + axis(b)).min(231) as u8
}

/// Vikunja's palette, resolved for one terminal.
///
/// The background is deliberately absent: a terminal application that paints its own
/// background loses the user's transparency, their colour scheme and their eyes' idea of
/// what a terminal looks like. tui-do paints foregrounds and selections only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Theme {
    depth: ColorDepth,
}

/// Vikunja's blue, which is the one colour the product is recognisable by.
const ACCENT: Shade = Shade::new((25, 115, 255), Color::Blue);
const MUTED: Shade = Shade::new((122, 128, 140), Color::DarkGray);
const BORDER: Shade = Shade::new((68, 74, 88), Color::DarkGray);
/// The selected row in the pane being driven. Bright enough to find at a glance in a
/// tiled window, dark enough that the row's own text stays readable on top of it.
const SELECTION_FOCUS: Shade = Shade::new((46, 82, 148), Color::Blue);

/// The selected row in a pane that is not being driven. Present, not competing.
const SELECTION_IDLE: Shade = Shade::new((38, 46, 64), Color::DarkGray);
const OVERDUE: Shade = Shade::new((224, 84, 84), Color::Red);
const SOON: Shade = Shade::new((230, 170, 60), Color::Yellow);
const OK: Shade = Shade::new((90, 190, 120), Color::Green);

impl Theme {
    /// The theme for a terminal of this depth.
    #[must_use]
    pub const fn new(depth: ColorDepth) -> Self {
        Self { depth }
    }

    /// Ordinary text: whatever the terminal's foreground is.
    #[must_use]
    pub const fn text(self) -> Style {
        Style::new()
    }

    /// Secondary text — counts, hints, empty values.
    #[must_use]
    pub fn muted(self) -> Style {
        Style::new().fg(MUTED.at(self.depth))
    }

    /// Vikunja blue, for the things the eye should land on.
    #[must_use]
    pub fn accent(self) -> Style {
        Style::new().fg(ACCENT.at(self.depth))
    }

    /// Pane borders and rules.
    #[must_use]
    pub fn border(self) -> Style {
        Style::new().fg(BORDER.at(self.depth))
    }

    /// A column heading or a section title.
    #[must_use]
    pub fn heading(self) -> Style {
        self.muted().add_modifier(Modifier::BOLD)
    }

    /// The selected row, brighter in the pane that has focus.
    ///
    /// Both states are a background colour, deliberately. The unfocused row used
    /// `REVERSED`, which swaps each *span's* own colour into its background — so a row
    /// carrying a due-soon date turned into a yellow bar, an overdue one into a red bar,
    /// and the labels into whatever the server had coloured them. The result was that
    /// the pane the user was *not* driving wore the loudest thing on the screen, while
    /// the focused pane got a quiet dark blue. The salience was backwards.
    #[must_use]
    pub fn selected(self, focused: bool) -> Style {
        if focused {
            Style::new()
                .bg(SELECTION_FOCUS.at(self.depth))
                .add_modifier(Modifier::BOLD)
        } else {
            // Visible, so the user can see where Tab will land them, but plainly quieter
            // than the pane they are driving.
            Style::new().bg(SELECTION_IDLE.at(self.depth))
        }
    }

    /// A pane's border or headings, accented while it has focus.
    ///
    /// The selected row answers "where am I", but not while a pane is empty or its
    /// selection is scrolled out of sight. This answers "which pane am I driving" on its
    /// own, which is the question Tab raises.
    #[must_use]
    pub fn pane(self, focused: bool) -> Style {
        if focused {
            Style::new().fg(ACCENT.at(self.depth))
        } else {
            self.border()
        }
    }

    /// A completed task.
    #[must_use]
    pub fn done(self) -> Style {
        self.muted().add_modifier(Modifier::CROSSED_OUT)
    }

    /// Something went wrong.
    #[must_use]
    pub fn error(self) -> Style {
        Style::new().fg(OVERDUE.at(self.depth))
    }

    /// Something the user should notice.
    #[must_use]
    pub fn warning(self) -> Style {
        Style::new().fg(SOON.at(self.depth))
    }

    /// Something worked.
    #[must_use]
    pub fn ok(self) -> Style {
        Style::new().fg(OK.at(self.depth))
    }

    /// How a priority reads.
    ///
    /// Vikunja only draws attention from 3 upwards, and so does tui-do: colouring "low"
    /// spends the user's attention on the tasks that least deserve it.
    #[must_use]
    pub fn priority(self, priority: i64) -> Style {
        match priority {
            5 => self.error().add_modifier(Modifier::BOLD),
            4 => self.error(),
            3 => self.warning(),
            _ => self.muted(),
        }
    }

    /// How a due date reads, relative to `now`.
    #[must_use]
    pub fn due(self, due: Option<DateTime<Utc>>, done: bool, now: DateTime<FixedOffset>) -> Style {
        let Some(due) = due else {
            return self.muted();
        };
        if done {
            return self.muted();
        }
        // Every question here is about the instant, not the calendar -- "is it past" and
        // "is it within 48 hours" mean the same thing in any zone -- so the offset is
        // dropped rather than honoured, unlike in `relative_date`.
        let now = now.with_timezone(&Utc);
        if due < now {
            return self.error();
        }
        if (due - now).num_hours() < 48 {
            return self.warning();
        }
        self.text()
    }

    /// A label chip, coloured the way the server says.
    ///
    /// The foreground is chosen from the background's luminance rather than fixed, so a
    /// pale yellow label is not white-on-white — which is what makes user-chosen colours
    /// safe to honour at all.
    #[must_use]
    pub fn label(self, hex: &str) -> Style {
        let Some((r, g, b)) = parse_hex(hex) else {
            return self.accent();
        };
        if self.depth == ColorDepth::None {
            return self.text();
        }
        let background = Shade::new((r, g, b), Color::Blue).at(self.depth);
        let foreground = if luminance(r, g, b) > 0.55 {
            Color::Black
        } else {
            Color::White
        };
        Style::new().bg(background).fg(foreground)
    }
}

/// Six hex digits, with or without the leading `#`. Vikunja omits it.
#[must_use]
pub fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let component = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    Some((component(0)?, component(2)?, component(4)?))
}

/// Perceived brightness, 0.0 to 1.0.
fn luminance(r: u8, g: u8, b: u8) -> f32 {
    // The sRGB coefficients, which weight green far above blue because eyes do.
    (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
    }

    /// The same instant as the reader's clock shows it.
    fn here() -> DateTime<FixedOffset> {
        now().fixed_offset()
    }

    #[test]
    fn no_colour_means_no_colour() {
        let theme = Theme::new(ColorDepth::None);
        assert_eq!(theme.error().fg, Some(Color::Reset));
        assert_eq!(theme.label("e05454"), theme.text());
    }

    #[test]
    fn a_shallow_terminal_gets_an_ansi_name_not_an_rgb_triple() {
        assert_eq!(Theme::new(ColorDepth::Ansi16).error().fg, Some(Color::Red));
        assert!(matches!(
            Theme::new(ColorDepth::Ansi256).error().fg,
            Some(Color::Indexed(_))
        ));
        assert_eq!(
            Theme::new(ColorDepth::TrueColor).error().fg,
            Some(Color::Rgb(224, 84, 84))
        );
    }

    #[test]
    fn a_near_grey_maps_to_the_grey_ramp_rather_than_a_tinted_cube_colour() {
        // 232..=255 is the greyscale ramp; 16..=231 is the colour cube.
        assert!((232..=255).contains(&index_for((122, 128, 140))));
        assert!((16..=231).contains(&index_for((224, 84, 84))));
    }

    #[test]
    fn a_hex_colour_survives_the_missing_hash_and_a_bad_one_falls_back() {
        assert_eq!(parse_hex("e05454"), Some((224, 84, 84)));
        assert_eq!(parse_hex("#e05454"), Some((224, 84, 84)));
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("nonsense"), None);

        let theme = Theme::new(ColorDepth::TrueColor);
        assert_eq!(theme.label(""), theme.accent());
    }

    #[test]
    fn a_pale_label_gets_dark_text_and_a_dark_one_gets_pale_text() {
        let theme = Theme::new(ColorDepth::TrueColor);
        assert_eq!(theme.label("ffee55").fg, Some(Color::Black));
        assert_eq!(theme.label("1d3f8f").fg, Some(Color::White));
    }

    #[test]
    fn a_due_date_reads_by_how_soon_it_is_and_a_done_task_stops_shouting() {
        let theme = Theme::new(ColorDepth::TrueColor);
        let yesterday = now() - chrono::Duration::days(1);
        let tomorrow = now() + chrono::Duration::days(1);
        let next_month = now() + chrono::Duration::days(30);

        assert_eq!(theme.due(Some(yesterday), false, here()), theme.error());
        assert_eq!(theme.due(Some(tomorrow), false, here()), theme.warning());
        assert_eq!(theme.due(Some(next_month), false, here()), theme.text());
        assert_eq!(theme.due(None, false, here()), theme.muted());

        // An overdue task that is finished is not overdue.
        assert_eq!(theme.due(Some(yesterday), true, here()), theme.muted());
    }

    #[test]
    fn only_the_priorities_worth_noticing_are_coloured() {
        let theme = Theme::new(ColorDepth::TrueColor);
        assert_eq!(theme.priority(0), theme.muted());
        assert_eq!(theme.priority(2), theme.muted());
        assert_ne!(theme.priority(3), theme.muted());
        assert_ne!(theme.priority(5), theme.priority(4));
    }
}
