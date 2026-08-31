//! Turning tasks into drawable rows.
//!
//! Pure arithmetic and text: given the tasks, the layout and a width, produce the cells
//! and the height of every row. No terminal is involved, so the width distribution and
//! the wrapping are unit-testable on their own — and the scroll maths in `update` reads
//! the same heights the renderer draws.
//!
//! Only the visible window is laid out. A 1,900-row query must not re-wrap 1,900 titles
//! to draw thirty of them.

use chrono::{DateTime, Datelike, FixedOffset, Utc};
use ratatui::text::{Line, Span};
use tui_do_core::config::columns::{Column, ColumnLayout, ColumnSpec};
use tui_do_core::models::{Project, Task, TaskId};
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// The space between two columns.
pub const GAP: u16 = 1;

/// The most lines one row may take, however much its title wraps.
///
/// A cap rather than "as many as it needs": with unbounded heights, a page motion and a
/// scrollbar both stop meaning anything, and one pathological title pushes everything
/// else off the screen.
pub const MAX_ROW_LINES: u16 = 3;

/// The narrowest a column is ever squeezed to before it is dropped instead.
const FLOOR: u16 = 4;

/// One column, measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasuredColumn {
    /// Which field.
    pub column: Column,
    /// Its heading.
    pub heading: String,
    /// How wide it is drawn.
    pub width: u16,
    /// Whether its text wraps.
    pub wrap: bool,
}

/// A row, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedRow {
    /// Which task it is, so the selection can be found without an index.
    pub task: TaskId,
    /// One entry per measured column, each already wrapped to its width.
    pub cells: Vec<Vec<Line<'static>>>,
    /// How many terminal lines it occupies.
    pub height: u16,
}

/// The width a column asks for before any space is shared out.
const fn natural_min(column: Column) -> u16 {
    match column {
        Column::Title => 20,
        Column::Project | Column::Assignees | Column::Created | Column::Updated => 10,
        Column::Labels | Column::Identifier => 8,
        Column::DueDate | Column::StartDate => 11,
        Column::Priority | Column::Status | Column::PercentDone => 4,
    }
}

/// Share `available` columns of terminal out between the layout's columns.
///
/// Minimums first, then maximums are respected, then whatever is left goes to the columns
/// that did not name a maximum — with the title taking the remainder, because a truncated
/// title costs more than a truncated project name. When even the minimums do not fit,
/// columns are dropped from the right rather than squeezed into illegibility; the title
/// is never dropped.
#[must_use]
pub fn measure(layout: &ColumnLayout, available: u16) -> Vec<MeasuredColumn> {
    let specs: Vec<&ColumnSpec> = layout.columns.iter().collect();
    if specs.is_empty() || available == 0 {
        return Vec::new();
    }

    let want = |spec: &ColumnSpec| -> u16 {
        let base = spec.width_percent.map_or_else(
            || spec.min_width.unwrap_or_else(|| natural_min(spec.column)),
            |percent| available.saturating_mul(percent.min(100)) / 100,
        );
        let base = base.max(spec.min_width.unwrap_or(0));
        spec.max_width.map_or(base, |max| base.min(max))
    };

    let mut kept: Vec<&ColumnSpec> = specs.clone();
    let mut widths: Vec<u16> = kept.iter().map(|spec| want(spec)).collect();

    // Drop from the right until the minimums fit, keeping the title whatever happens.
    while kept.len() > 1 && total(&widths, kept.len()) > available {
        let droppable = kept
            .iter()
            .rposition(|spec| spec.column != Column::Title)
            .unwrap_or(0);
        kept.remove(droppable);
        widths.remove(droppable);
    }

    // A single column that still does not fit is squeezed rather than dropped: something
    // has to be drawn.
    if total(&widths, kept.len()) > available {
        if let Some(width) = widths.first_mut() {
            *width = available.max(FLOOR);
        }
    }

    let mut spare = available.saturating_sub(total(&widths, kept.len()));

    // Every column that can still grow grows by the same amount; what a column with a
    // maximum cannot take falls to the title. Growing the capped columns to their maximum
    // *first* is the obvious rule and it is wrong: on a narrow list it hands twenty
    // columns to the labels while the title stays at its minimum and wraps to three
    // lines. A truncated title costs the reader more than a truncated project name.
    if spare > 0 {
        let growable: Vec<usize> = kept
            .iter()
            .enumerate()
            .filter(|(index, spec)| spec.max_width.is_none_or(|max| widths[*index] < max))
            .map(|(index, _)| index)
            .collect();
        if !growable.is_empty() {
            let share = spare / growable.len() as u16;
            for index in &growable {
                let room = kept[*index]
                    .max_width
                    .map_or(share, |max| share.min(max.saturating_sub(widths[*index])));
                widths[*index] += room;
                spare -= room;
            }
        }
        if spare > 0 {
            let flexible: Vec<usize> = kept
                .iter()
                .enumerate()
                .filter(|(_, spec)| spec.max_width.is_none())
                .map(|(index, _)| index)
                .collect();
            let title = kept
                .iter()
                .position(|spec| spec.column == Column::Title)
                .filter(|index| flexible.contains(index));
            // No flexible column means the layout asked for a fixed total; the slack is
            // left as slack rather than pushing a column past the maximum it named.
            if let Some(index) = title.or_else(|| flexible.first().copied()) {
                widths[index] += spare;
            }
        }
    }

    kept.into_iter()
        .zip(widths)
        .map(|(spec, width)| MeasuredColumn {
            column: spec.column,
            heading: spec.heading().to_string(),
            width,
            wrap: spec.wrap,
        })
        .collect()
}

fn total(widths: &[u16], count: usize) -> u16 {
    widths.iter().sum::<u16>() + GAP * (count.saturating_sub(1)) as u16
}

/// What a row needs to know about the world outside the task.
#[derive(Debug, Clone, Copy)]
pub struct RowContext<'a> {
    /// Every project, for the project column.
    pub projects: &'a [Project],
    /// The colours.
    pub theme: Theme,
    /// The time the runtime last reported.
    pub now: DateTime<FixedOffset>,
}

/// Lay out `tasks` into rows.
#[must_use]
pub fn render(
    tasks: &[Task],
    columns: &[MeasuredColumn],
    context: RowContext<'_>,
) -> Vec<RenderedRow> {
    let marked = says_priority_in_the_title(columns);
    tasks
        .iter()
        .map(|task| {
            let cells: Vec<Vec<Line<'static>>> = columns
                .iter()
                .map(|column| cell(task, column, context, marked))
                .collect();
            let height = height_of(&cells);
            RenderedRow {
                task: task.id,
                cells,
                height,
            }
        })
        .collect()
}

/// How tall a row is, from cells that have already been laid out.
fn height_of(cells: &[Vec<Line<'static>>]) -> u16 {
    cells
        .iter()
        .map(|lines| lines.len() as u16)
        .max()
        .unwrap_or(1)
        .clamp(1, MAX_ROW_LINES)
}

/// How tall one task's row would be, without keeping the row.
///
/// The scroll maths needs heights and nothing else. Going through the same `cell` code
/// the renderer uses is the point: a second, cheaper estimate is exactly how the list
/// came to show seven tasks while the selection was on the eighteenth.
#[must_use]
pub fn row_height(task: &Task, columns: &[MeasuredColumn], context: RowContext<'_>) -> u16 {
    let marked = says_priority_in_the_title(columns);
    let cells: Vec<Vec<Line<'static>>> = columns
        .iter()
        .map(|column| cell(task, column, context, marked))
        .collect();
    height_of(&cells)
}

/// Whether these columns leave the title to say what the priority is.
///
/// A layout carrying `Pri` says it in its own column, and saying it twice on one row is
/// noise. `measure` drops columns from the right when they do not fit, so a terminal too
/// narrow to keep `Pri` gets the marker back -- which is what should happen: the fact
/// does not stop mattering because the window got small.
fn says_priority_in_the_title(columns: &[MeasuredColumn]) -> bool {
    !columns
        .iter()
        .any(|column| column.column == Column::Priority)
}

/// How a high priority reads in the title: `!!!` at High, one more `!` per step above it.
///
/// `None` below High, because [`Theme::priority`] already draws nothing there -- colouring
/// "low" spends the reader's attention on the tasks that least deserve it -- and `None`
/// for a finished task, the same way [`Theme::due`] mutes a done task's date. The urgency
/// was about getting it done.
fn priority_marker(task: &Task) -> Option<String> {
    if task.done || task.priority < 3 {
        return None;
    }
    let steps = usize::try_from(task.priority.min(5)).unwrap_or(3);
    Some("!".repeat(steps))
}

/// How many whole rows fit in `height` lines, starting at `first`.
///
/// What a page motion should move by. Never zero: a row taller than the screen still
/// counts as one, or paging would stop moving.
#[must_use]
pub fn fit(
    tasks: &[Task],
    columns: &[MeasuredColumn],
    context: RowContext<'_>,
    first: usize,
    height: u16,
) -> usize {
    let mut used = 0;
    let mut count = 0;
    for task in tasks.iter().skip(first) {
        let row = row_height(task, columns, context);
        if count > 0 && used + row > height {
            break;
        }
        used += row;
        count += 1;
    }
    count.max(1)
}

/// The largest offset that still shows the row at `index` in full.
///
/// Walks back from the selected row, adding heights until one more would not fit. At
/// most `height` rows are measured, because no row is shorter than a line.
#[must_use]
pub fn first_visible(
    tasks: &[Task],
    columns: &[MeasuredColumn],
    context: RowContext<'_>,
    index: usize,
    height: u16,
) -> usize {
    let mut used = 0;
    let mut first = index;
    for candidate in (0..=index).rev() {
        let Some(task) = tasks.get(candidate) else {
            continue;
        };
        let row = row_height(task, columns, context);
        // The selected row is kept whatever its height: one taller than the whole body
        // is drawn from its top and clipped, which is what the renderer does too.
        if candidate < index && used + row > height {
            break;
        }
        used += row;
        first = candidate;
    }
    first
}

/// One cell, wrapped or truncated to its column's width.
///
/// `marked` is whether the title carries the priority marker; see
/// [`says_priority_in_the_title`].
fn cell(
    task: &Task,
    column: &MeasuredColumn,
    context: RowContext<'_>,
    marked: bool,
) -> Vec<Line<'static>> {
    let theme = context.theme;
    let width = column.width;

    // A high priority is said in the title itself rather than in a gutter of its own, so
    // the marked title is the one that gives up the room and the rows without a priority
    // -- the majority -- pay nothing for it. Two spans, so the marker keeps the colour
    // the theme gives a priority while the title keeps its own.
    if column.column == Column::Title && marked {
        if let Some(mark) = priority_marker(task) {
            let text = format!("{mark} {}", task.title);
            let lines = if column.wrap {
                wrap(&text, width, MAX_ROW_LINES)
            } else {
                vec![truncate(&text, width)]
            };
            return lines
                .into_iter()
                .enumerate()
                .map(|(index, line)| {
                    // Only the first line opens with the marker, and only when the width
                    // left it whole -- a title squeezed to nothing keeps its own colour
                    // rather than showing half a marker in the priority's.
                    if index == 0 {
                        if let Some(rest) = line.strip_prefix(mark.as_str()) {
                            return Line::from(vec![
                                Span::styled(mark.clone(), theme.priority(task.priority)),
                                Span::styled(rest.to_string(), theme.text()),
                            ]);
                        }
                    }
                    Line::from(Span::styled(line, theme.text()))
                })
                .collect();
        }
    }

    // Labels are the one cell that is not a single run of text: each chip carries the
    // colour the server gave it.
    if column.column == Column::Labels {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut used = 0u16;
        for label in &task.labels {
            let chip = format!(" {} ", label.title);
            let chip_width = display_width(&chip);
            if used + chip_width > width {
                if spans.is_empty() {
                    // Nothing fits whole, so show the first label cut short rather than a
                    // lone ellipsis, which tells the reader there are labels but not which.
                    spans.push(Span::styled(
                        truncate(&chip, width),
                        theme.label(&label.hex_color),
                    ));
                } else if used < width {
                    spans.push(Span::styled("…".to_string(), theme.muted()));
                }
                break;
            }
            spans.push(Span::styled(chip, theme.label(&label.hex_color)));
            used += chip_width;
        }
        return vec![Line::from(spans)];
    }

    let (text, style) = match column.column {
        Column::Title => (
            task.title.clone(),
            if task.done {
                theme.done()
            } else {
                theme.text()
            },
        ),
        Column::Project => (
            context
                .projects
                .iter()
                .find(|project| project.id == task.project_id)
                .map_or_else(String::new, |project| project.title.clone()),
            theme.muted(),
        ),
        Column::DueDate => (
            relative_date(task.due_date.get(), context.now),
            theme.due(task.due_date.get(), task.done, context.now),
        ),
        Column::StartDate => (
            relative_date(task.start_date.get(), context.now),
            theme.muted(),
        ),
        Column::Priority => (
            if task.has_priority() {
                format!("P{}", task.priority)
            } else {
                String::new()
            },
            theme.priority(task.priority),
        ),
        Column::Status => (
            if task.done {
                "✓".to_string()
            } else {
                String::new()
            },
            theme.ok(),
        ),
        Column::Assignees => (
            task.assignees
                .iter()
                .map(|user| user.display_name().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            theme.muted(),
        ),
        Column::Created => (
            relative_date(task.created.get(), context.now),
            theme.muted(),
        ),
        Column::Updated => (
            relative_date(task.updated.get(), context.now),
            theme.muted(),
        ),
        Column::Identifier => (task.display_identifier(), theme.muted()),
        Column::PercentDone => (
            if task.percent_done > 0.0 {
                format!("{:.0}%", task.percent_done * 100.0)
            } else {
                String::new()
            },
            theme.muted(),
        ),
        Column::Labels => (String::new(), theme.text()),
    };

    let lines = if column.wrap {
        wrap(&text, width, MAX_ROW_LINES)
    } else {
        vec![truncate(&text, width)]
    };
    lines
        .into_iter()
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

/// How many terminal cells a string occupies.
#[must_use]
pub fn display_width(text: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX)
}

/// Cut `text` to `width`, ending in an ellipsis when something was lost.
#[must_use]
pub fn truncate(text: &str, width: u16) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0u16;
    for c in text.chars() {
        let w = display_width(&c.to_string());
        if used + w > width - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Wrap `text` at word boundaries into at most `max_lines` lines.
///
/// An explicit newline is a hard break. It has to be: `split_whitespace` treats `\n` as
/// ordinary space, so without this a description typed as two paragraphs rendered as one
/// run-on line -- and pressing Enter in the edit form looked like it did nothing at all,
/// because the character went in and was then drawn as a space.
///
/// The last line is truncated and ellipsised rather than the overflow being dropped
/// silently: a cell that quietly loses half a title is worse than one that says it did.
#[must_use]
pub fn wrap(text: &str, width: u16, max_lines: u16) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return vec![String::new()];
    }
    if text.contains('\n') {
        let mut lines: Vec<String> = Vec::new();
        for paragraph in text.split('\n') {
            let room = max_lines.saturating_sub(u16::try_from(lines.len()).unwrap_or(u16::MAX));
            if room == 0 {
                break;
            }
            if paragraph.trim().is_empty() {
                lines.push(String::new());
                continue;
            }
            lines.extend(wrap(paragraph, width, room));
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        return lines;
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut overflowed = false;

    for word in text.split_whitespace() {
        if lines.len() as u16 >= max_lines {
            overflowed = true;
            break;
        }
        let mut word = word.to_string();

        // A single word wider than the column is broken rather than looping forever.
        if display_width(&word) > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            while display_width(&word) > width && (lines.len() as u16) < max_lines - 1 {
                let head: String = word
                    .chars()
                    .scan(0u16, |used, c| {
                        *used += display_width(&c.to_string());
                        (*used <= width).then_some(c)
                    })
                    .collect();
                word = word[head.len()..].to_string();
                lines.push(head);
            }
            current = word;
            continue;
        }

        let candidate = if current.is_empty() {
            word.clone()
        } else {
            format!("{current} {word}")
        };
        if display_width(&candidate) <= width {
            current = candidate;
            continue;
        }
        lines.push(std::mem::take(&mut current));
        if lines.len() as u16 >= max_lines {
            overflowed = true;
            break;
        }
        current = word;
    }

    if !current.is_empty() {
        if (lines.len() as u16) < max_lines {
            lines.push(truncate(&current, width));
        } else {
            overflowed = true;
        }
    }
    if overflowed {
        if let Some(last) = lines.last_mut() {
            *last = truncate(&format!("{last} …"), width);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// What Vikunja calls a priority, so tui-do and the web UI say the same word.
///
/// The list column shows `P3` because it has four characters to work with; anywhere with
/// room -- the picker, the quick-action menu, a toast -- says "High" instead, because
/// nobody should have to remember which end of the scale is urgent.
#[must_use]
pub const fn priority_name(priority: i64) -> &'static str {
    match priority {
        1 => "Low",
        2 => "Medium",
        3 => "High",
        4 => "Urgent",
        5 => "DO NOW",
        _ => "Unset",
    }
}

/// A due date as a person would say it.
#[must_use]
pub fn relative_date(date: Option<DateTime<Utc>>, now: DateTime<FixedOffset>) -> String {
    let Some(date) = date else {
        return "—".to_string();
    };
    // Into the user's zone before asking what day it is. "Today" is a claim about the
    // calendar the user is living in, and a due date stored as 2026-08-27T04:59Z is the
    // evening of the 26th for a UTC-5 reader -- comparing UTC days calls that "Tomorrow"
    // and then, at midnight UTC, "Today" while the evening it names has already gone.
    let date = date.with_timezone(&now.timezone());
    let days = (date.date_naive() - now.date_naive()).num_days();
    match days {
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        -1 => "Yesterday".to_string(),
        2..=6 => format!("in {days} days"),
        -6..=-2 => format!("{} days ago", -days),
        _ if date.year() == now.year() => date.format("%b %-d").to_string(),
        _ => date.format("%b %-d, %y").to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tui_do_core::config::columns::ColumnSpec;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
    }

    /// The same instant as the reader's clock shows it. Zero offset, so the existing
    /// expectations below are unchanged; `a_date_is_read_in_the_users_own_zone` is the
    /// one that puts a real offset on it.
    fn here() -> DateTime<FixedOffset> {
        now().fixed_offset()
    }

    fn layout(columns: Vec<ColumnSpec>) -> ColumnLayout {
        ColumnLayout {
            name: "test".into(),
            description: None,
            columns,
        }
    }

    #[test]
    fn the_spare_space_goes_to_the_title() {
        let measured = measure(
            &layout(vec![
                ColumnSpec {
                    min_width: Some(25),
                    ..ColumnSpec::new(Column::Title)
                },
                ColumnSpec {
                    min_width: Some(10),
                    max_width: Some(15),
                    ..ColumnSpec::new(Column::Project)
                },
                ColumnSpec {
                    min_width: Some(10),
                    max_width: Some(12),
                    ..ColumnSpec::new(Column::DueDate)
                },
            ]),
            120,
        );
        let widths: Vec<u16> = measured.iter().map(|column| column.width).collect();
        assert_eq!(widths, vec![91, 15, 12]);
        assert_eq!(widths.iter().sum::<u16>() + GAP * 2, 120);
    }

    #[test]
    fn a_narrow_terminal_drops_columns_from_the_right_and_keeps_the_title() {
        let measured = measure(
            &layout(vec![
                ColumnSpec::new(Column::Title),
                ColumnSpec::new(Column::Project),
                ColumnSpec::new(Column::Labels),
                ColumnSpec::new(Column::DueDate),
            ]),
            34,
        );
        let kept: Vec<Column> = measured.iter().map(|column| column.column).collect();
        assert_eq!(kept, vec![Column::Title, Column::Project]);
        assert!(measured.iter().map(|c| c.width).sum::<u16>() + GAP <= 34);
    }

    #[test]
    fn a_column_that_leads_with_something_other_than_the_title_still_keeps_it() {
        let measured = measure(
            &layout(vec![
                ColumnSpec::new(Column::Identifier),
                ColumnSpec::new(Column::Title),
                ColumnSpec::new(Column::Assignees),
            ]),
            30,
        );
        assert!(measured.iter().any(|column| column.column == Column::Title));
    }

    #[test]
    fn a_percentage_width_is_still_honoured() {
        let measured = measure(
            &layout(vec![
                ColumnSpec {
                    width_percent: Some(50),
                    ..ColumnSpec::new(Column::Title)
                },
                ColumnSpec {
                    width_percent: Some(25),
                    max_width: Some(25),
                    ..ColumnSpec::new(Column::DueDate)
                },
            ]),
            100,
        );
        assert_eq!(measured[1].width, 25);
        // The title is flexible, so it absorbs what the percentages left over.
        assert_eq!(measured[0].width + measured[1].width + GAP, 100);
    }

    #[test]
    fn truncation_marks_that_something_was_lost() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a longer title", 8), "a longe…");
        assert_eq!(truncate("anything", 1), "…");
        assert_eq!(truncate("anything", 0), "");
    }

    #[test]
    fn wrapping_breaks_at_words_and_caps_the_row() {
        let lines = wrap("fix the authentication token refresh bug", 12, 3);
        assert_eq!(lines, vec!["fix the", "authenticati", "on token …"]);
        assert!(lines.len() <= 3);
    }

    #[test]
    fn a_word_longer_than_the_column_is_broken_rather_than_looping() {
        let lines = wrap("supercalifragilistic", 6, 3);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| display_width(line) <= 6));
    }

    #[test]
    fn width_is_measured_in_terminal_cells_not_characters() {
        // Two-cell characters, so four characters occupy eight columns.
        assert_eq!(display_width("日本語だ"), 8);
        assert_eq!(display_width("abcd"), 4);
        assert!(display_width(&truncate("日本語だよ", 6)) <= 6);
    }

    #[test]
    fn a_date_is_read_in_the_users_own_zone() {
        // 2026-08-27T02:00Z is still the evening of the 26th in New York. A reader there,
        // at 21:00 on the 26th, is looking at something due in five hours -- "Today". Ask
        // the question in UTC and the two instants land on different calendar days, so
        // the row says "Tomorrow" about this evening. The same mislabelling is what makes
        // a task due tonight read as overdue at midnight UTC rather than at midnight.
        let eastern = FixedOffset::west_opt(5 * 3600).unwrap();
        // 21:00 on the 26th where the reader is; 02:00 on the 27th in UTC.
        let evening = Utc.with_ymd_and_hms(2026, 8, 27, 2, 0, 0).unwrap();

        let before = eastern.with_ymd_and_hms(2026, 8, 26, 16, 0, 0).unwrap();
        assert_eq!(relative_date(Some(evening), before), "Today");

        // And the day turns over at the reader's midnight, not at UTC's: an hour past it,
        // that same evening is behind them.
        let after = eastern.with_ymd_and_hms(2026, 8, 27, 1, 0, 0).unwrap();
        assert_eq!(relative_date(Some(evening), after), "Yesterday");
    }

    #[test]
    fn a_date_reads_the_way_a_person_would_say_it() {
        assert_eq!(relative_date(None, here()), "—");
        assert_eq!(relative_date(Some(now()), here()), "Today");
        assert_eq!(
            relative_date(Some(now() + chrono::Duration::days(1)), here()),
            "Tomorrow"
        );
        assert_eq!(
            relative_date(Some(now() + chrono::Duration::days(3)), here()),
            "in 3 days"
        );
        assert_eq!(
            relative_date(Some(now() - chrono::Duration::days(3)), here()),
            "3 days ago"
        );
        assert_eq!(
            relative_date(Some(now() + chrono::Duration::days(30)), here()),
            "Sep 23"
        );
        assert_eq!(
            relative_date(Some(now() + chrono::Duration::days(400)), here()),
            "Sep 28, 27"
        );
    }

    #[test]
    fn a_rows_height_is_the_tallest_cell_capped() {
        let theme = Theme::new(crate::theme::ColorDepth::TrueColor);
        let columns = measure(
            &layout(vec![ColumnSpec {
                min_width: Some(10),
                max_width: Some(10),
                wrap: true,
                ..ColumnSpec::new(Column::Title)
            }]),
            10,
        );
        let task = Task {
            id: TaskId(1),
            title: "one two three four five six seven".to_string(),
            ..Task::default()
        };
        let rows = render(
            &[task],
            &columns,
            RowContext {
                projects: &[],
                theme,
                now: here(),
            },
        );
        assert_eq!(rows[0].height, MAX_ROW_LINES);
        assert_eq!(rows[0].task, TaskId(1));
    }

    /// Eighteen tasks whose titles all wrap to the cap.
    fn wrapping(count: i64) -> Vec<Task> {
        (1..=count)
            .map(|n| Task {
                id: TaskId(n),
                title: format!("task {n} one two three four five six seven"),
                ..Task::default()
            })
            .collect()
    }

    fn narrow() -> Vec<MeasuredColumn> {
        measure(
            &layout(vec![ColumnSpec {
                min_width: Some(10),
                max_width: Some(10),
                wrap: true,
                ..ColumnSpec::new(Column::Title)
            }]),
            10,
        )
    }

    fn plain_context() -> RowContext<'static> {
        RowContext {
            projects: &[],
            theme: Theme::new(crate::theme::ColorDepth::TrueColor),
            now: here(),
        }
    }

    #[test]
    fn what_fits_is_counted_in_rows_not_lines() {
        // The whole bug: twelve lines of room, but every row is three lines tall, so
        // four tasks fit -- not twelve.
        let tasks = wrapping(18);
        let context = plain_context();
        assert_eq!(fit(&tasks, &narrow(), context, 0, 12), 4);
    }

    #[test]
    fn a_selection_below_the_fold_pulls_the_offset_down_to_it() {
        let tasks = wrapping(18);
        let context = plain_context();
        // Four rows fit, so showing the twelfth means starting at the ninth.
        assert_eq!(first_visible(&tasks, &narrow(), context, 11, 12), 8);
        assert_eq!(first_visible(&tasks, &narrow(), context, 17, 12), 14);
    }

    #[test]
    fn a_row_taller_than_the_body_is_still_shown() {
        // Otherwise the walk backwards finds nothing that fits and the list sticks.
        let tasks = wrapping(4);
        let context = plain_context();
        assert_eq!(first_visible(&tasks, &narrow(), context, 2, 1), 2);
        assert_eq!(fit(&tasks, &narrow(), context, 2, 1), 1);
    }
    /// The title cell's first line, span by span.
    fn title_line(task: Task, columns: &[MeasuredColumn]) -> Vec<(String, ratatui::style::Style)> {
        let index = columns
            .iter()
            .position(|column| column.column == Column::Title)
            .unwrap();
        let rows = render(&[task], columns, plain_context());
        rows[0].cells[index]
            .first()
            .unwrap()
            .spans
            .iter()
            .map(|span| (span.content.to_string(), span.style))
            .collect()
    }

    fn prioritised(priority: i64) -> Task {
        Task {
            id: TaskId(1),
            title: "Renew the domain before it lapses".to_string(),
            priority,
            ..Task::default()
        }
    }

    fn title_only(width: u16) -> Vec<MeasuredColumn> {
        measure(&layout(vec![ColumnSpec::new(Column::Title)]), width)
    }

    #[test]
    fn a_high_priority_is_said_in_the_title_and_a_low_one_is_not() {
        let columns = title_only(40);
        // Below High, nothing at all. Colouring "low" spends the reader's attention on
        // the tasks that least deserve it, which is the rule `Theme::priority` already
        // states and the rule the web UI follows.
        for priority in [0, 1, 2] {
            let spans = title_line(prioritised(priority), &columns);
            assert_eq!(spans.len(), 1, "priority {priority} marked the title");
            assert_eq!(spans[0].0, "Renew the domain before it lapses");
        }
        // From High up, one `!` per step, in the colour the theme already gives it.
        for (priority, mark) in [(3, "!!!"), (4, "!!!!"), (5, "!!!!!")] {
            let spans = title_line(prioritised(priority), &columns);
            assert_eq!(spans[0].0, mark);
            assert_eq!(spans[0].1, plain_context().theme.priority(priority));
            assert_eq!(spans[1].0, " Renew the domain before it lapses");
        }
    }

    #[test]
    fn a_finished_task_stops_shouting() {
        // The same reasoning `Theme::due` uses when it mutes a done task's date: the
        // urgency was about getting it done, and it is done.
        let spans = title_line(
            Task {
                done: true,
                ..prioritised(5)
            },
            &title_only(40),
        );
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, "Renew the domain before it lapses");
    }

    #[test]
    fn the_marker_stands_down_when_the_layout_already_shows_the_priority() {
        // Both at once says it twice on one row. If the terminal is too narrow to keep
        // the Pri column, `measure` drops it and the marker comes back -- which is the
        // behaviour wanted, not an accident.
        let columns = measure(
            &layout(vec![
                ColumnSpec::new(Column::Priority),
                ColumnSpec::new(Column::Title),
            ]),
            40,
        );
        assert!(columns
            .iter()
            .any(|column| column.column == Column::Priority));
        let spans = title_line(prioritised(5), &columns);
        assert_eq!(spans.len(), 1);
        assert!(spans[0].0.starts_with("Renew the domain"));
    }

    #[test]
    fn the_marker_is_paid_for_out_of_the_title_it_marks() {
        // It lives inside the title cell rather than in a gutter of its own, so the
        // marked title gives up the room and every other row pays nothing.
        let columns = title_only(20);
        let spans = title_line(prioritised(3), &columns);
        assert_eq!(spans[0].0, "!!!");
        assert_eq!(display_width(&spans[1].0), 17);
        assert!(spans[1].0.ends_with('\u{2026}'));

        let plain = title_line(prioritised(0), &columns);
        assert_eq!(plain.len(), 1);
        assert_eq!(display_width(&plain[0].0), 20);
    }
}

/// Lay a value out for an edit box, and say where the caret lands.
///
/// Character-wrapped rather than word-wrapped, and deliberately: in a box the user is
/// typing into, a caret offset has to map to exactly one cell, and re-flowing words
/// breaks that mapping. Explicit newlines are hard breaks.
///
/// Returns at most `max_lines` rows, windowed so the caret is always among them, and the
/// caret's position *within that window* as (row, column).
#[must_use]
pub fn edit_layout(
    text: &str,
    width: u16,
    max_lines: u16,
    cursor: usize,
) -> (Vec<String>, (u16, u16)) {
    if width == 0 || max_lines == 0 {
        return (vec![String::new()], (0, 0));
    }
    let mut rows: Vec<String> = Vec::new();
    let mut caret = (0_u16, 0_u16);
    let mut seen = 0_usize;

    for (paragraph_index, paragraph) in text.split('\n').enumerate() {
        if paragraph_index > 0 {
            seen += 1; // the newline itself
        }
        let mut current = String::new();
        let mut used = 0_u16;
        for (offset, c) in paragraph.chars().enumerate() {
            let w = display_width(&c.to_string()).max(1);
            if used + w > width {
                rows.push(std::mem::take(&mut current));
                used = 0;
            }
            if seen + offset == cursor {
                caret = (u16::try_from(rows.len()).unwrap_or(u16::MAX), used);
            }
            current.push(c);
            used += w;
        }
        let chars = paragraph.chars().count();
        if seen + chars == cursor {
            caret = (u16::try_from(rows.len()).unwrap_or(u16::MAX), used);
        }
        seen += chars;
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }

    // Window the rows so the caret is visible, keeping the box a constant height.
    let max = usize::from(max_lines);
    let first = usize::from(caret.0).saturating_sub(max.saturating_sub(1));
    let window: Vec<String> = rows.into_iter().skip(first).take(max).collect();
    let caret_row = caret.0 - u16::try_from(first).unwrap_or(0);
    (window, (caret_row, caret.1))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod edit_layout_tests {
    use super::*;

    #[test]
    fn an_explicit_newline_is_a_hard_break() {
        // `split_whitespace` ate these, so a two-paragraph description drew as one line
        // and Enter in the form looked like it did nothing.
        assert_eq!(wrap("one\ntwo", 40, 4), vec!["one", "two"]);
        assert_eq!(wrap("one\n\ntwo", 40, 4), vec!["one", "", "two"]);
    }

    #[test]
    fn a_hard_break_still_word_wraps_each_paragraph() {
        assert_eq!(wrap("aaa bbb\nccc ddd", 7, 4), vec!["aaa bbb", "ccc ddd"]);
    }

    #[test]
    fn the_caret_lands_on_the_row_the_newline_made() {
        let (rows, caret) = edit_layout("one\ntwo", 40, 4, 4);
        assert_eq!(rows, vec!["one", "two"]);
        assert_eq!(
            caret,
            (1, 0),
            "just after the newline is the start of row two"
        );
    }

    #[test]
    fn the_caret_at_the_end_sits_past_the_last_character() {
        let (_, caret) = edit_layout("one\ntwo", 40, 4, 7);
        assert_eq!(caret, (1, 3));
    }

    #[test]
    fn a_line_wider_than_the_box_continues_on_the_next_row() {
        let (rows, caret) = edit_layout("abcdef", 3, 4, 4);
        assert_eq!(rows, vec!["abc", "def"]);
        assert_eq!(caret, (1, 1));
    }

    #[test]
    fn the_window_follows_the_caret_past_the_bottom() {
        // Five rows of content in a four-row box: the box scrolls rather than hiding the
        // line being typed on.
        let text = "a\nb\nc\nd\ne";
        let (rows, caret) = edit_layout(text, 40, 4, text.chars().count());
        assert_eq!(rows, vec!["b", "c", "d", "e"]);
        assert_eq!(caret, (3, 1));
    }
}
