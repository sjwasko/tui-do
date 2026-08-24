//! Drawing the model.
//!
//! `view` only reads. Every decision it makes is already in the model — which pane has
//! focus, what the store answered, whether a query is still in flight — so a screenshot
//! of criax is a pure function of a `Model` and a size, and the golden tests exercise it
//! without a terminal.

use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::keymap::{bindings_in, Group};
use crate::modal::{Modal, PickerState, TextInput};
use crate::model::{Focus, Level, Model, SyncStatus};
use crate::query::Scope;
use crate::rows::{self, MeasuredColumn, RowContext};
use crate::sidebar::{self, SidebarRow};
use crate::theme::Theme;

/// Draw everything.
pub fn view(model: &Model, frame: &mut Frame) {
    let frames = model.frames();
    header(model, frame, frames.header);
    if let Some(area) = frames.sidebar {
        draw_sidebar(model, frame, area);
    }
    list(model, frame, frames.list);
    if let Some(area) = frames.preview {
        preview(model, frame, area);
    }
    status(model, frame, frames.status);

    // The stack draws bottom-up, so the modal the keyboard is talking to is the one on
    // top -- the same order `update` routes keys in.
    for modal in &model.modals {
        draw_modal(model, modal, frame);
    }
}

/// Breadcrumb, view tabs and the sync indicator.
fn header(model: &Model, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let theme = model.theme;
    let mut left = vec![
        Span::styled("criax", theme.accent().add_modifier(Modifier::BOLD)),
        Span::styled("  ", theme.text()),
        Span::styled(breadcrumb(model), theme.text().add_modifier(Modifier::BOLD)),
        Span::styled(" ", theme.text()),
    ];
    // Only List is reachable today; Table and Kanban arrive with the views API, and the
    // strip is here from the start so they land in a place rather than a redesign.
    left.push(Span::styled("› ", theme.muted()));
    left.push(Span::styled(
        "List",
        theme.text().add_modifier(Modifier::UNDERLINED),
    ));
    left.push(Span::styled("  Table  Kanban", theme.muted()));

    let right = sync_indicator(model);
    let left = fit(left, area.width.saturating_sub(line_width(&right)));
    let used = line_width(&left) + line_width(&right);
    let padding = usize::from(area.width.saturating_sub(used));
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(padding)));
    spans.extend(right);

    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(usize::from(area.width)),
                theme.border(),
            ))),
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
        );
    }
}

/// What the list is showing, in words.
fn breadcrumb(model: &Model) -> String {
    match model.query.scope {
        Scope::All => "All tasks".to_string(),
        Scope::Favorites => "Favorites".to_string(),
        Scope::Project(id) => model
            .project(id)
            .map_or_else(|| format!("Project {id}"), |project| project.title.clone()),
        Scope::Label(id) => model
            .data
            .labels
            .iter()
            .find(|label| label.id == id)
            .map_or_else(
                || format!("Label {id}"),
                |label| format!("#{}", label.title),
            ),
    }
}

/// The right-hand end of the header: what sync is doing.
fn sync_indicator(model: &Model) -> Vec<Span<'static>> {
    let theme = model.theme;
    match &model.status.sync {
        SyncStatus::Working { detail } => {
            vec![Span::styled(format!("⟳ {detail} "), theme.accent())]
        }
        SyncStatus::Failed { message } => vec![Span::styled(
            format!("⚠ offline — {} ", rows::truncate(message, 30)),
            theme.error(),
        )],
        SyncStatus::Idle => {
            let text = match model.status.last_sync {
                Some(at) => format!("⟳ synced {} ", ago(model.now - at)),
                None => "⟳ not synced yet ".to_string(),
            };
            vec![Span::styled(text, theme.muted())]
        }
    }
}

/// A duration, the way a status line says it.
fn ago(elapsed: chrono::Duration) -> String {
    let seconds = elapsed.num_seconds().max(0);
    match seconds {
        0..=45 => "just now".to_string(),
        46..=5400 => format!("{}m ago", (seconds + 30) / 60),
        _ => format!("{}h ago", (seconds + 1800) / 3600),
    }
}

/// The project tree.
fn draw_sidebar(model: &Model, frame: &mut Frame, area: Rect) {
    let theme = model.theme;
    let focused = model.focus == Focus::Sidebar;
    let block = Block::new()
        .borders(Borders::RIGHT)
        .border_style(theme.border());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let all = sidebar::rows(&model.data.projects, &model.data.counts, &model.sidebar);
    let lines: Vec<Line<'static>> = all
        .iter()
        .skip(model.sidebar.offset)
        .take(usize::from(inner.height))
        .map(|row| sidebar_line(row, model, focused, inner.width))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn sidebar_line(row: &SidebarRow, model: &Model, focused: bool, width: u16) -> Line<'static> {
    let theme = model.theme;
    match row {
        SidebarRow::Heading(text) => Line::from(Span::styled((*text).to_string(), theme.heading())),
        SidebarRow::Target {
            target,
            title,
            open,
            depth,
            has_children,
            collapsed,
        } => {
            let marker = if *has_children {
                if *collapsed {
                    "▸ "
                } else {
                    "▾ "
                }
            } else {
                "  "
            };
            let indent = "  ".repeat(usize::from(*depth));
            // Trailing space so the count does not sit against the pane's border.
            let count = if *open > 0 {
                format!(" {open} ")
            } else {
                "  ".to_string()
            };
            let room = width
                .saturating_sub(rows::display_width(&indent))
                .saturating_sub(rows::display_width(marker))
                .saturating_sub(rows::display_width(&count));
            let label = rows::truncate(title, room);
            let pad = usize::from(room.saturating_sub(rows::display_width(&label)));

            let selected = model.sidebar.selected == *target;
            let base = if selected {
                theme.selected(focused)
            } else {
                theme.text()
            };
            let line = Line::from(vec![
                Span::styled(format!("{indent}{marker}{label}"), base),
                Span::styled(" ".repeat(pad), base),
                Span::styled(count, if selected { base } else { theme.muted() }),
            ]);
            if selected {
                line.style(base)
            } else {
                line
            }
        }
    }
}

/// The task list: column headings, then rows.
fn list(model: &Model, frame: &mut Frame, area: Rect) {
    let theme = model.theme;
    if area.height == 0 || area.width < 2 {
        return;
    }
    // A column of gutter, so text never sits against the sidebar's border.
    let area = Rect {
        x: area.x + 1,
        width: area.width - 1,
        ..area
    };
    let columns = rows::measure(model.layout(), area.width);

    let heading = Line::from(interleave(
        columns
            .iter()
            .map(|column| {
                Span::styled(
                    pad(&rows::truncate(&column.heading, column.width), column.width),
                    theme.heading(),
                )
            })
            .collect(),
    ));
    frame.render_widget(Paragraph::new(heading), Rect { height: 1, ..area });

    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    if body.height == 0 {
        return;
    }

    if let Some(message) = empty_message(model) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(message, theme.muted()))),
            body,
        );
        return;
    }

    // Only the visible window is laid out. Everything below is arithmetic on rows that
    // will actually be drawn.
    let window: Vec<_> = model
        .data
        .tasks
        .iter()
        .skip(model.list.offset)
        .take(usize::from(body.height))
        .cloned()
        .collect();
    let rendered = rows::render(
        &window,
        &columns,
        RowContext {
            projects: &model.data.projects,
            theme,
            now: model.now,
        },
    );

    let focused = model.focus == Focus::List;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for row in &rendered {
        let selected = model.list.selected == Some(row.task);
        for mut line in row_lines(row, &columns) {
            if selected {
                line = line.style(theme.selected(focused));
            }
            lines.push(line);
        }
        if lines.len() >= usize::from(body.height) {
            break;
        }
    }
    lines.truncate(usize::from(body.height));
    frame.render_widget(Paragraph::new(lines), body);
}

/// What to say when there is nothing to draw.
///
/// "Loading" and "nothing here" are different facts and the user can tell them apart.
fn empty_message(model: &Model) -> Option<String> {
    if let Some(error) = &model.data.error {
        return Some(format!("Could not read the local store: {error}"));
    }
    if !model.data.tasks.is_empty() {
        return None;
    }
    Some(if model.data.loading {
        "Loading…".to_string()
    } else if model.query.search.is_some() {
        "Nothing matches that search.".to_string()
    } else if model.query.include_done {
        "No tasks here.".to_string()
    } else {
        "No open tasks here. Press t to include completed ones.".to_string()
    })
}

/// One row's worth of lines, cells laid side by side.
fn row_lines(row: &rows::RenderedRow, columns: &[MeasuredColumn]) -> Vec<Line<'static>> {
    (0..row.height)
        .map(|index| {
            let spans: Vec<Vec<Span<'static>>> = columns
                .iter()
                .enumerate()
                .map(|(column_index, column)| {
                    let mut cell = row
                        .cells
                        .get(column_index)
                        .and_then(|lines| lines.get(usize::from(index)))
                        .cloned()
                        .unwrap_or_default()
                        .spans;
                    let used: u16 = cell
                        .iter()
                        .map(|span| rows::display_width(&span.content))
                        .sum();
                    if used < column.width {
                        cell.push(Span::raw(" ".repeat(usize::from(column.width - used))));
                    }
                    cell
                })
                .collect();
            Line::from(interleave_all(spans))
        })
        .collect()
}

/// The selected task, in more detail than a row can hold.
fn preview(model: &Model, frame: &mut Frame, area: Rect) {
    let theme = model.theme;
    let block = Block::new()
        .borders(Borders::LEFT)
        .border_style(theme.border());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(task) = model.selected_task() else {
        return;
    };
    let width = inner.width.saturating_sub(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for line in rows::wrap(&task.title, width, 3) {
        lines.push(Line::from(Span::styled(
            format!(" {line}"),
            theme.text().add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(Span::styled(
        format!(" {}", "─".repeat(usize::from(width))),
        theme.border(),
    )));

    let project = model
        .project(task.project_id)
        .map_or_else(String::new, |project| project.title.clone());
    lines.push(field(" Project", &project, theme));
    lines.push(field(" ID", &task.display_identifier(), theme));
    lines.push(field(
        " Due",
        &rows::relative_date(task.due_date.get(), model.now),
        theme,
    ));
    if task.has_priority() {
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<9}", "Priority"), theme.muted()),
            Span::styled(format!("P{}", task.priority), theme.priority(task.priority)),
        ]));
    }
    if !task.labels.is_empty() {
        let mut spans = vec![Span::styled(format!(" {:<9}", "Labels"), theme.muted())];
        for label in &task.labels {
            spans.push(Span::styled(
                format!(" {} ", label.title),
                theme.label(&label.hex_color),
            ));
        }
        lines.push(Line::from(spans));
    }
    if !task.assignees.is_empty() {
        let names: Vec<String> = task
            .assignees
            .iter()
            .map(|user| user.display_name().to_string())
            .collect();
        lines.push(field(" Assignees", &names.join(", "), theme));
    }

    if !task.description.trim().is_empty() {
        lines.push(Line::default());
        // Descriptions are HTML from the web editor. Phase 5 renders them properly, via
        // `glow` with a built-in fallback; until then the text is shown as it is stored
        // rather than pretended to be Markdown.
        for line in rows::wrap(&task.description, width, 40) {
            lines.push(Line::from(Span::styled(format!(" {line}"), theme.text())));
        }
    }

    let scrolled: Vec<Line<'static>> = lines
        .into_iter()
        .skip(usize::from(model.list.preview_scroll))
        .collect();
    frame.render_widget(Paragraph::new(scrolled), inner);
}

fn field(name: &str, value: &str, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<10}"), theme.muted()),
        Span::styled(value.to_string(), theme.text()),
    ])
}

/// Counts, queued changes and the key hints.
fn status(model: &Model, frame: &mut Frame, area: Rect) {
    let theme = model.theme;
    if area.height == 0 {
        return;
    }
    let left = match &model.status.toast {
        Some(toast) => vec![Span::styled(
            toast.text.clone(),
            match toast.level {
                Level::Info => theme.accent(),
                Level::Warning => theme.warning(),
                Level::Error => theme.error(),
            },
        )],
        None => {
            let mut spans = vec![Span::styled(
                format!("{} tasks", model.data.tasks.len()),
                theme.muted(),
            )];
            if model.data.truncated {
                spans.push(Span::styled(" (list capped)", theme.warning()));
            }
            if model.status.queued > 0 {
                spans.push(Span::styled(
                    format!(" · {} queued", model.status.queued),
                    theme.warning(),
                ));
            }
            if model.query.search.is_some() {
                spans.push(Span::styled(" · filtered", theme.accent()));
            }
            spans
        }
    };

    let hint = if model.pending.is_empty() {
        "?:help  /:search  g:go  q:quit".to_string()
    } else {
        // A chord in flight is visible, so a half-pressed `g` is never a mystery.
        model
            .pending
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
            + " …"
    };
    let right = vec![Span::styled(hint, theme.muted())];
    // The hints are fixed and the message is not, so the message is what gives way.
    let left = fit(left, area.width.saturating_sub(line_width(&right) + 1));
    let padding = usize::from(
        area.width
            .saturating_sub(line_width(&left) + line_width(&right)),
    );
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(padding)));
    spans.extend(right);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draw one modal over whatever is underneath.
fn draw_modal(model: &Model, modal: &Modal, frame: &mut Frame) {
    let theme = model.theme;
    let (width, height) = match modal {
        Modal::Help(_) => (64, 22),
        Modal::Search(_) => (60, 3),
        Modal::Picker(_) => (60, 16),
    };
    let area = centered(frame.area(), width, height);
    let block = Block::bordered()
        .border_style(theme.accent())
        .title(modal.title());
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    match modal {
        Modal::Help(state) => {
            let mut lines: Vec<Line<'static>> = Vec::new();
            for group in Group::all() {
                lines.push(Line::from(Span::styled(
                    group.heading().to_string(),
                    theme.heading(),
                )));
                for binding in bindings_in(*group, state.context) {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {:<12}", binding.keys_display()), theme.accent()),
                        Span::styled(binding.doc.to_string(), theme.text()),
                    ]));
                }
                lines.push(Line::default());
            }
            let scrolled: Vec<Line<'static>> = lines.into_iter().skip(state.offset).collect();
            frame.render_widget(Paragraph::new(scrolled), inner);
        }
        Modal::Search(input) => {
            frame.render_widget(Paragraph::new(input_line(input, theme)), inner);
            place_cursor(frame, inner, input, 2);
        }
        Modal::Picker(picker) => {
            picker_body(picker, frame, inner, theme);
        }
    }
}

fn input_line(input: &TextInput, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("> ", theme.accent()),
        Span::styled(input.value().to_string(), theme.text()),
    ])
}

fn place_cursor(frame: &mut Frame, area: Rect, input: &TextInput, prefix: u16) {
    let x = area.x + prefix + input.cursor() as u16;
    if x < area.right() {
        frame.set_cursor_position((x, area.y));
    }
}

fn picker_body(picker: &PickerState, frame: &mut Frame, area: Rect, theme: Theme) {
    let mut lines = vec![input_line(&picker.input, theme)];
    for (position, index) in picker.matches.iter().enumerate() {
        let Some(candidate) = picker.candidates.get(*index) else {
            continue;
        };
        let style = if position == picker.selected {
            theme.selected(true)
        } else {
            theme.text()
        };
        lines.push(Line::from(Span::styled(
            format!(" {}", candidate.title),
            style,
        )));
    }
    if picker.matches.is_empty() {
        lines.push(Line::from(Span::styled(
            " nothing matches".to_string(),
            theme.muted(),
        )));
    }
    lines.truncate(usize::from(area.height));
    frame.render_widget(Paragraph::new(lines), area);
    place_cursor(frame, area, &picker.input, 2);
}

/// A box of this size in the middle of `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn pad(text: &str, width: u16) -> String {
    let used = rows::display_width(text);
    format!(
        "{text}{}",
        " ".repeat(usize::from(width.saturating_sub(used)))
    )
}

/// Cut a run of spans down to `width`, ellipsising whatever is left in the middle of a
/// span. A status message longer than the terminal must not walk over the key hints.
fn fit(spans: Vec<Span<'static>>, width: u16) -> Vec<Span<'static>> {
    if line_width(&spans) <= width {
        return spans;
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0u16;
    for span in spans {
        let span_width = rows::display_width(&span.content);
        if used + span_width <= width {
            used += span_width;
            out.push(span);
            continue;
        }
        let room = width.saturating_sub(used);
        if room > 0 {
            let style = span.style;
            out.push(Span::styled(rows::truncate(&span.content, room), style));
        }
        break;
    }
    out
}

fn line_width(spans: &[Span<'_>]) -> u16 {
    spans
        .iter()
        .map(|span| rows::display_width(&span.content))
        .sum()
}

/// Join cells with a single space between them.
fn interleave(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    interleave_all(spans.into_iter().map(|span| vec![span]).collect())
}

fn interleave_all(cells: Vec<Vec<Span<'static>>>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for (index, cell) in cells.into_iter().enumerate() {
        if index > 0 {
            out.push(Span::raw(" ".repeat(usize::from(rows::GAP))));
        }
        out.extend(cell);
    }
    out
}
