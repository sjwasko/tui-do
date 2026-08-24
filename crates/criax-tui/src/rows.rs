//! Turning tasks into drawable rows.
//!
//! Pure arithmetic and text: given the tasks, the layout and a width, produce the cells
//! and the height of every row. No terminal is involved, so the width distribution and
//! the wrapping are unit-testable on their own — and the scroll maths in `update` reads
//! the same heights the renderer draws.
//!
//! Only the visible window is laid out. A 1,900-row query must not re-wrap 1,900 titles
//! to draw thirty of them.

use chrono::{DateTime, Datelike, Utc};
use criax_core::config::columns::{Column, ColumnLayout, ColumnSpec};
use criax_core::models::{Project, Task, TaskId};
use ratatui::text::{Line, Span};
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
    pub now: DateTime<Utc>,
}

/// Lay out `tasks` into rows.
#[must_use]
pub fn render(
    tasks: &[Task],
    columns: &[MeasuredColumn],
    context: RowContext<'_>,
) -> Vec<RenderedRow> {
    tasks
        .iter()
        .map(|task| {
            let cells: Vec<Vec<Line<'static>>> = columns
                .iter()
                .map(|column| cell(task, column, context))
                .collect();
            let height = cells
                .iter()
                .map(|lines| lines.len() as u16)
                .max()
                .unwrap_or(1)
                .clamp(1, MAX_ROW_LINES);
            RenderedRow {
                task: task.id,
                cells,
                height,
            }
        })
        .collect()
}

/// One cell, wrapped or truncated to its column's width.
fn cell(task: &Task, column: &MeasuredColumn, context: RowContext<'_>) -> Vec<Line<'static>> {
    let theme = context.theme;
    let width = column.width;

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
/// The last line is truncated and ellipsised rather than the overflow being dropped
/// silently: a cell that quietly loses half a title is worse than one that says it did.
#[must_use]
pub fn wrap(text: &str, width: u16, max_lines: u16) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return vec![String::new()];
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

/// Turn the HTML Vikunja's editor stores into something readable.
///
/// A stopgap, and labelled as one: Phase 5 pipes descriptions through `glow` with a
/// `pulldown-cmark` fallback and gets links, emphasis and code blocks right. Until then
/// the alternative is not "plain text" but `<p><a target="_blank" rel="noopener"` on
/// screen, which is what the first run against real data actually showed.
///
/// Block-level tags become newlines and list items get a bullet, so the shape of a
/// description survives even though its formatting does not.
#[must_use]
pub fn plain_text(html: &str) -> String {
    /// Tags after which a line break belongs.
    const BLOCK: &[&str] = &[
        "p",
        "div",
        "br",
        "li",
        "tr",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "blockquote",
        "pre",
        "ul",
        "ol",
        "table",
    ];

    let mut out = String::new();
    let mut tag = String::new();
    let mut in_tag = false;
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if in_tag {
            if c == '>' {
                in_tag = false;
                let closing = tag.starts_with('/');
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if BLOCK.contains(&name.as_str()) {
                    // Both `<p>` and `</p>` mean "a line ends here", and a nested
                    // `</li></ul>` means it twice. One break is what was meant -- and
                    // real markup puts whitespace between block tags, so the trailing
                    // spaces have to go before that test means anything.
                    while out.ends_with(' ') || out.ends_with('\t') {
                        out.pop();
                    }
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    if name == "li" && !closing {
                        out.push_str("• ");
                    }
                }
                tag.clear();
            } else {
                tag.push(c);
            }
            continue;
        }
        // Only a `<` followed by a name or a slash starts a tag, so prose containing
        // "a < b" is not silently eaten.
        if c == '<'
            && chars
                .peek()
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == '/')
        {
            in_tag = true;
            continue;
        }
        out.push(c);
    }

    let out = decode_entities(&out);

    // Collapse the whitespace the markup left behind, but keep paragraph breaks.
    let mut text = String::new();
    let mut blank_run = 0;
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 || text.is_empty() {
                continue;
            }
        } else {
            blank_run = 0;
        }
        text.push_str(&line);
        text.push('\n');
    }
    text.trim_end().to_string()
}

/// The handful of entities that actually appear in Vikunja descriptions.
fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        // Last, so a literal "&amp;lt;" does not decode twice into "<".
        .replace("&amp;", "&")
}

/// A due date as a person would say it.
#[must_use]
pub fn relative_date(date: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(date) = date else {
        return "—".to_string();
    };
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
    use criax_core::config::columns::ColumnSpec;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap()
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
    fn a_date_reads_the_way_a_person_would_say_it() {
        assert_eq!(relative_date(None, now()), "—");
        assert_eq!(relative_date(Some(now()), now()), "Today");
        assert_eq!(
            relative_date(Some(now() + chrono::Duration::days(1)), now()),
            "Tomorrow"
        );
        assert_eq!(
            relative_date(Some(now() + chrono::Duration::days(3)), now()),
            "in 3 days"
        );
        assert_eq!(
            relative_date(Some(now() - chrono::Duration::days(3)), now()),
            "3 days ago"
        );
        assert_eq!(
            relative_date(Some(now() + chrono::Duration::days(30)), now()),
            "Sep 23"
        );
        assert_eq!(
            relative_date(Some(now() + chrono::Duration::days(400)), now()),
            "Sep 28, 27"
        );
    }

    #[test]
    fn html_from_the_web_editor_reads_as_text() {
        let html = "<p>The refresh call needs the cookie set by                     <a target=\"_blank\" rel=\"noopener\" href=\"http://x\">POST /login</a>.</p>                    <ul><li>one</li><li>two</li></ul>";
        assert_eq!(
            plain_text(html),
            "The refresh call needs the cookie set by POST /login.\n• one\n• two"
        );
    }

    #[test]
    fn entities_decode_once_and_plain_prose_is_left_alone() {
        assert_eq!(plain_text("Tom &amp; Jerry &lt;3"), "Tom & Jerry <3");
        assert_eq!(plain_text("&amp;lt; stays escaped"), "&lt; stays escaped");
        assert_eq!(plain_text("just a note"), "just a note");
        // Prose, not markup: `<` followed by a space starts no tag.
        assert_eq!(plain_text("a < b and b > c"), "a < b and b > c");
    }

    #[test]
    fn paragraph_breaks_survive_but_the_blank_run_does_not() {
        assert_eq!(plain_text("<p>one</p><p></p><p></p><p>two</p>"), "one\ntwo");
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
                now: now(),
            },
        );
        assert_eq!(rows[0].height, MAX_ROW_LINES);
        assert_eq!(rows[0].task, TaskId(1));
    }
}
