//! Drawing the model.
//!
//! `view` only reads. Every decision it makes is already in the model — which pane has
//! focus, what the store answered, whether a query is still in flight — so a screenshot
//! of tui-do is a pure function of a `Model` and a size, and the golden tests exercise it
//! without a terminal.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::keymap::{help_rows, HelpRow};
use crate::modal::{
    ConfirmLabelsState, DueState, EditField, EditState, LabelEditState, LabelField, LabelsState,
    Modal, PickerState, PriorityState, QuickActionsState, SearchState, TextInput, MAX_PRIORITY,
};
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
///
/// Everything here is optional except the breadcrumb and the sync state. On a phone
/// terminal — Termux at 45 columns is the case this was written against — the tabs and
/// the brand are the first things to go, in that order, because "which list am I looking
/// at" and "is it talking to the server" are the only two questions the header answers
/// that the rest of the screen does not.
fn header(model: &Model, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let theme = model.theme;
    let right = sync_indicator(model);
    // At least one column of gap, so the two halves never abut.
    let room = area.width.saturating_sub(line_width(&right) + 1);

    let brand = || {
        vec![
            Span::styled("tui-do", theme.accent().add_modifier(Modifier::BOLD)),
            Span::styled("  ", theme.text()),
        ]
    };
    let crumb = || {
        vec![Span::styled(
            breadcrumb(model),
            theme.text().add_modifier(Modifier::BOLD),
        )]
    };
    // Only List is reachable today; Table and Kanban arrive with the views API, and the
    // strip is here from the start so they land in a place rather than a redesign.
    let tabs = || {
        vec![
            Span::styled(" › ", theme.muted()),
            Span::styled("List", theme.text().add_modifier(Modifier::UNDERLINED)),
            Span::styled("  Table  Kanban", theme.muted()),
        ]
    };

    let mut left: Vec<Span<'static>> = [brand(), crumb(), tabs()].concat();
    if line_width(&left) > room {
        left = [brand(), crumb()].concat();
    }
    if line_width(&left) > room {
        left = crumb();
    }
    let left = fit(left, room);

    let padding = usize::from(
        area.width
            .saturating_sub(line_width(&left) + line_width(&right)),
    );
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
        .border_style(theme.pane(focused));
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

    // The list sits between the other two panes and owns no border of its own, so its
    // column headings are where it says whether it is the pane being driven.
    let heading_style = if model.focus == Focus::List {
        theme.pane(true).add_modifier(Modifier::BOLD)
    } else {
        theme.heading()
    };
    let heading = Line::from(interleave(
        columns
            .iter()
            .map(|column| {
                Span::styled(
                    pad(&rows::truncate(&column.heading, column.width), column.width),
                    heading_style,
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

/// The most lines one paragraph of a description may take before it is ellipsised.
const MAX_PARAGRAPH_LINES: u16 = 40;

/// The selected task, in more detail than a row can hold.
fn preview(model: &Model, frame: &mut Frame, area: Rect) {
    let theme = model.theme;
    let block = Block::new()
        .borders(Borders::LEFT)
        .border_style(theme.pane(model.focus == Focus::Preview));
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

    // Descriptions are HTML from the web editor. Phase 5 renders them properly, through
    // `glow` with a `pulldown-cmark` fallback; until then the tags are stripped, because
    // the alternative on screen is `<p><a target="_blank" rel="noopener"`.
    let description = rows::plain_text(&task.description);
    if !description.is_empty() {
        lines.push(Line::default());
        // Only what can be seen is laid out. Wrapping every paragraph of a long
        // description on every frame, to draw the dozen lines that fit, is the same
        // mistake the task list already avoids -- and a per-paragraph cap is no cap at
        // all, since a description has as many paragraphs as it likes.
        let budget = usize::from(model.list.preview_scroll) + usize::from(inner.height);
        for paragraph in description.lines() {
            if lines.len() >= budget {
                break;
            }
            for line in rows::wrap(paragraph, width, MAX_PARAGRAPH_LINES) {
                lines.push(Line::from(Span::styled(format!(" {line}"), theme.text())));
            }
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
    // A toast takes the whole line. The hints are always true and one `?` away; a
    // transient message is neither, and cutting it in half loses exactly the part that
    // tells the user what to do about it.
    if let Some(toast) = &model.status.toast {
        let style = match toast.level {
            Level::Info => theme.accent(),
            Level::Warning => theme.warning(),
            Level::Error => theme.error(),
        };
        let text = rows::truncate(&toast.text, area.width);
        frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
        return;
    }

    let left = match &model.status.toast {
        Some(_) => Vec::new(),
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
                // "3 queued" while the server is refusing all three reads as progress. It
                // is not, and the count alone never goes down to say so.
                if model.status.failing > 0 {
                    spans.push(Span::styled(
                        format!(" ({} failing)", model.status.failing),
                        theme.error(),
                    ));
                }
            }
            if model.query.search.is_some() {
                spans.push(Span::styled(" · filtered", theme.accent()));
            }
            spans
        }
    };

    let hint = if !model.pending.is_empty() {
        // A chord in flight is visible, so a half-pressed `g` is never a mystery.
        model
            .pending
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
            + " …"
    } else if model.query.search.is_some() {
        // The way out of a narrowed view belongs where the user looks for it, which is
        // not the help modal.
        "Esc:clear filter  ?:help  q:quit".to_string()
    } else {
        "?:help  /:search  g:go  q:quit".to_string()
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

    // The search is a prompt, not a box. It filters as it is typed, so a panel in the
    // middle of the screen would cover the very list the user is watching change.
    if let Modal::Search(state) = modal {
        search_prompt(model, state, frame);
        return;
    }
    if let Modal::Add(input) = modal {
        add_prompt(model, input, frame);
        return;
    }
    if let Modal::Due(state) = modal {
        due_prompt(model, state, frame);
        return;
    }

    let rows = match modal {
        Modal::Help(state) => help_rows(state.context),
        _ => Vec::new(),
    };
    let help = matches!(modal, Modal::Help(_)).then(|| help_layout(&rows, frame.area()));
    let (width, height) = match modal {
        // Sized to what it has to say, so adding a binding cannot silently push the last
        // one off the bottom -- which is exactly what binding Esc did. How wide, and
        // whether that is one column or two, depends on the terminal: see `help_layout`.
        Modal::Help(_) => help.map_or((64, 0), |layout| (layout.width, layout.height)),
        // Both draw as prompts and return before this is reached; the arm exists so
        // adding a modal is a compile error until it has been given a size.
        Modal::Search(_) | Modal::Add(_) | Modal::Due(_) => (60, 3),
        Modal::Picker(_) | Modal::Labels(_) => (60, 16),
        // Two fields and the line that says why the last Enter was refused, plus the
        // border. The third row is always drawn: it holds the hint when there is no
        // refusal, so the box does not resize under the user as they type.
        Modal::LabelEdit(_) => (60, 5),
        // One row per name the user typed and cannot have, plus the border, a blank line
        // and the row that says what is happening. That row is always drawn -- it holds
        // the reason before the answer and "Creating…" after it -- so the box does not
        // resize under a user who has just pressed a key.
        Modal::ConfirmLabels(state) => (
            60,
            u16::try_from(state.unknown.len())
                .unwrap_or(u16::MAX)
                .saturating_add(4),
        ),
        // One row per priority plus the field and the border: the whole range is on
        // screen at once, which is the point of a fixed scale.
        Modal::Priority(_) => (44, MAX_PRIORITY as u16 + 4),
        // Sized to what was configured, plus the border: a menu that cut off the last
        // quick action would be a menu that hid the key the user was reaching for.
        Modal::QuickActions(state) => (48, state.rows.len() as u16 + 2),
        // One line per field, the description given room to be a description, plus the
        // border and the hint. Sized to its content for the same reason the help modal
        // is: a fixed height is how a row goes missing.
        Modal::Edit(_) => (72, EditField::ALL.len() as u16 + EDIT_DESCRIPTION_LINES + 3),
    };
    let area = centered(frame.area(), width, height);
    let truncated = help.is_some_and(|layout| area.height < layout.height);
    let title = if truncated {
        // On a terminal too short to hold it, the title says so rather than leaving the
        // reader to guess that the list continues.
        format!("{} — j/k scrolls", modal.title())
    } else {
        modal.title()
    };
    let block = Block::bordered().border_style(theme.accent()).title(title);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    match modal {
        Modal::Help(state) => {
            let layout = help.unwrap_or_else(|| help_layout(&rows, frame.area()));
            help_body(&rows, layout, state.offset, frame, inner, theme);
        }
        // Drawn as prompts above, never as boxes.
        Modal::Search(_) | Modal::Add(_) | Modal::Due(_) => {}
        Modal::Edit(state) => edit_body(state, frame, inner, theme),
        Modal::Picker(picker) => {
            picker_body(picker, frame, inner, theme);
        }
        Modal::Priority(state) => priority_body(state, frame, inner, theme),
        Modal::Labels(state) => labels_body(state, frame, inner, theme),
        Modal::LabelEdit(state) => label_edit_body(state, frame, inner, theme),
        Modal::ConfirmLabels(state) => confirm_labels_body(state, frame, inner, theme),
        Modal::QuickActions(state) => quick_actions_body(state, frame, inner, theme),
    }
}

/// The gap between the help modal's two columns.
const HELP_GUTTER: u16 = 3;
/// The two spaces a binding's keys are indented by.
const HELP_INDENT: usize = 2;
/// What a binding's keys are padded out to before its description begins.
const HELP_KEYS_WIDTH: usize = 12;
/// The narrowest a help column is drawn, which is what it was before it could be two.
const HELP_MIN_COLUMN: u16 = 62;

/// How the help modal is laid out.
///
/// The one decision in this file that depends on the terminal rather than only on the
/// model, which is why it is a value computed here and not a field somewhere: `view` is
/// still a pure function of a `Model` and a size.
#[derive(Debug, Clone, Copy)]
struct HelpLayout {
    /// Where the second column starts, or `None` for a single column.
    split: Option<usize>,
    /// Outer width, borders included.
    width: u16,
    /// Outer height, borders included.
    height: u16,
}

/// Choose between one column and two.
///
/// One column is the better read whenever it fits: the bindings run top to bottom in the
/// order the keymap declares them, and nothing has to be looked for twice. Two is for
/// when it does not — thirty-five bindings and four headings need forty-four rows, so a
/// forty-row terminal scrolls, and a *help* screen is the worst place to have to scroll
/// to find `q  Quit`. A hundred and twenty columns has room for both halves side by
/// side, so on that terminal the whole reference is on screen at once.
fn help_layout(rows: &[HelpRow], screen: Rect) -> HelpLayout {
    let column = help_column_width(rows);
    let single = HelpLayout {
        split: None,
        // Never narrower than the single column always was: a keymap that lost its
        // longest description should not make the box visibly shrink. The folded form
        // takes the measured width instead, because two columns at the floor would not
        // fit the terminal this exists for.
        width: column.max(HELP_MIN_COLUMN) + 2,
        height: u16::try_from(rows.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2),
    };
    if single.height <= screen.height {
        return single;
    }
    let width = column * 2 + HELP_GUTTER + 2;
    if width > screen.width {
        // Too narrow to fold, so it scrolls — which is what the title says it does.
        return single;
    }
    let split = help_split(rows);
    let (left, right) = help_columns(rows, split);
    if right.is_empty() {
        return single;
    }
    HelpLayout {
        split: Some(split),
        width,
        height: u16::try_from(left.len().max(right.len()))
            .unwrap_or(u16::MAX)
            .saturating_add(2),
    }
}

/// How wide one column of help has to be to hold every row whole.
fn help_column_width(rows: &[HelpRow]) -> u16 {
    let widest = rows
        .iter()
        .map(|row| match row {
            HelpRow::Heading(text) => text.chars().count(),
            HelpRow::Binding(binding) => {
                HELP_INDENT
                    + binding.keys_display().chars().count().max(HELP_KEYS_WIDTH)
                    + binding.doc.chars().count()
            }
            HelpRow::Blank => 0,
        })
        .max()
        .unwrap_or(0);
    u16::try_from(widest).unwrap_or(u16::MAX)
}

/// Where to fold the rows into two columns.
///
/// Only ever at a heading, so a section is never cut in half across the gutter — the
/// reader would have no way to tell that the four bindings at the top of the right
/// column belong to the heading at the foot of the left one.
fn help_split(rows: &[HelpRow]) -> usize {
    let mut best = rows.len();
    let mut closest = usize::MAX;
    for (index, row) in rows.iter().enumerate().skip(1) {
        if !matches!(row, HelpRow::Heading(_)) {
            continue;
        }
        let distance = index.abs_diff(rows.len() - index);
        if distance < closest {
            closest = distance;
            best = index;
        }
    }
    best
}

/// The rows either side of the fold.
fn help_columns(rows: &[HelpRow], split: usize) -> (&[HelpRow], &[HelpRow]) {
    let (left, right) = rows.split_at(split.min(rows.len()));
    // The blank that separated the two sections is the gutter now, and leaving it at the
    // foot of the left column would make the box a row taller for nothing.
    let left = match left.last() {
        Some(HelpRow::Blank) => &left[..left.len() - 1],
        _ => left,
    };
    (left, right)
}

/// Draw the help modal's rows, in one column or two.
fn help_body(
    rows: &[HelpRow],
    layout: HelpLayout,
    offset: usize,
    frame: &mut Frame,
    area: Rect,
    theme: Theme,
) {
    let Some(split) = layout.split else {
        help_column(rows, offset, frame, area, theme);
        return;
    };
    let (left, right) = help_columns(rows, split);
    let width = area.width.saturating_sub(HELP_GUTTER) / 2;
    help_column(left, offset, frame, Rect { width, ..area }, theme);
    help_column(
        right,
        offset,
        frame,
        Rect {
            x: area.x + width + HELP_GUTTER,
            width: area.width.saturating_sub(width + HELP_GUTTER),
            ..area
        },
        theme,
    );
}

/// Draw one column of help, scrolled to `offset`.
fn help_column(rows: &[HelpRow], offset: usize, frame: &mut Frame, area: Rect, theme: Theme) {
    // Clamped to what is actually left below, so scrolling stops with the last row at the
    // foot of the box rather than carrying on until the box is empty. The modal's own
    // bound counts every row, which is right for one column and one column too many for
    // two.
    let last = rows.len().saturating_sub(area.height as usize);
    let lines: Vec<Line<'static>> = rows
        .iter()
        .skip(offset.min(last))
        .map(|row| match row {
            HelpRow::Heading(text) => {
                Line::from(Span::styled((*text).to_string(), theme.heading()))
            }
            HelpRow::Binding(binding) => Line::from(vec![
                Span::styled(
                    format!(
                        "{:indent$}{:<width$}",
                        "",
                        binding.keys_display(),
                        indent = HELP_INDENT,
                        width = HELP_KEYS_WIDTH
                    ),
                    theme.accent(),
                ),
                Span::styled(binding.doc.to_string(), theme.text()),
            ]),
            HelpRow::Blank => Line::default(),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Where a scrolling list has to start so `selected` is on screen.
///
/// Every modal that holds a list is sized to its content, and `centered` then clips it to
/// whatever the terminal actually has. On a short window that clipping used to take the
/// bottom rows away with nothing to scroll them back: the highlight moved off the edge
/// and the arrow keys read as doing nothing at all, because the one thing on screen that
/// would have shown otherwise was gone.
fn scrolled_to(selected: usize, count: usize, height: usize) -> usize {
    if height == 0 || count <= height {
        return 0;
    }
    let last_start = count - height;
    // Keep the highlight one row inside the edge where there is room, so there is always
    // a hint that the list continues.
    selected.saturating_sub(height - 1).min(last_start)
}

/// The priority field: every level on screen, with the one in force marked.
fn priority_body(state: &PriorityState, frame: &mut Frame, area: Rect, theme: Theme) {
    let mut lines = vec![Line::from(vec![
        Span::styled("> ", theme.accent()),
        Span::styled(state.typed.clone(), theme.text()),
    ])];
    // The field occupies the first line, so the rows have one less than the box.
    let rows = usize::from(area.height).saturating_sub(1);
    let first = scrolled_to(state.selected.clamp(0, MAX_PRIORITY) as usize, 6, rows);
    for value in (first as i64)..=MAX_PRIORITY {
        let selected = value == state.selected;
        let style = if selected {
            theme.selected(true)
        } else {
            // Priority 4 and 5 are the colours the list column uses, so the scale reads
            // the same here as it does on the row.
            theme.priority(value)
        };
        let hint = if value == state.current {
            "current"
        } else {
            ""
        };
        let left = format!(" {value}  {}", rows::priority_name(value));
        let room = area.width.saturating_sub(rows::display_width(hint) + 2);
        let gap = usize::from(room.saturating_sub(rows::display_width(&left))) + 1;
        lines.push(Line::from(vec![
            Span::styled(left, style),
            Span::styled(" ".repeat(gap), style),
            Span::styled(
                hint.to_string(),
                if selected { style } else { theme.muted() },
            ),
        ]));
    }
    lines.truncate(usize::from(area.height));
    frame.render_widget(Paragraph::new(lines), area);
    let cursor = state.typed.chars().count() as u16;
    if area.x + 2 + cursor < area.right() {
        frame.set_cursor_position((area.x + 2 + cursor, area.y));
    }
}

/// The due-date prompt, on the status line.
///
/// A prompt rather than a box for the same reason the search is one: the task it is about
/// is in the list, and a panel in the middle of the screen would cover it. The right-hand
/// side shows what the parser made of the text, so `next friday` can be checked before
/// Enter commits to it.
fn due_prompt(model: &Model, state: &DueState, frame: &mut Frame) {
    let theme = model.theme;
    let area = model.frames().status;
    if area.height == 0 {
        return;
    }

    let typed = state.input.value().trim();
    let right = if typed.is_empty() {
        vec![Span::styled(
            "empty clears the date   Enter:set  Esc:cancel",
            theme.muted(),
        )]
    } else {
        match tui_do_core::quickadd::parse(typed, &model.now).due_date {
            Some(due) => vec![
                Span::styled(
                    rows::relative_date(Some(due), model.now),
                    theme.due(Some(due), false, model.now),
                ),
                Span::styled("  Enter:set  C-u:clear  Esc:cancel", theme.muted()),
            ],
            // Named as not-understood while it is still being typed, because half of
            // `tomorrow` is not a date either and the user is mid-word.
            None => vec![Span::styled(
                "not a date yet   C-u:clear  Esc:cancel",
                theme.muted(),
            )],
        }
    };
    let left = vec![
        Span::styled("due ", theme.accent()),
        Span::styled(state.input.value().to_string(), theme.text()),
    ];
    let padding = usize::from(
        area.width
            .saturating_sub(line_width(&left) + line_width(&right)),
    );
    let mut spans = fit(left, area.width.saturating_sub(line_width(&right) + 1));
    spans.push(Span::raw(" ".repeat(padding)));
    spans.extend(right);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    place_cursor(frame, area, &state.input, 4);
}

/// The footer of the label form: the two keys the modal handles itself.
///
/// The only place either is ever advertised. Both are modal-local -- the modal stack
/// takes every key before `KEYMAP` is consulted -- so the help modal, which is rendered
/// from that table, cannot know about them. Each is shown exactly when it would do
/// something, which is also when the user is looking for it: `C-n` when what has been
/// typed is a name no label has, `C-e` when there is a row under the cursor to rename.
fn labels_offer(state: &LabelsState, theme: Theme, width: u16) -> Option<Line<'static>> {
    /// What each hint is keyed and worded as. The pair is the single source of both the
    /// spans below and the width they are measured at, so a rewording cannot leave the
    /// truncation sized for the old text.
    const CREATE: (&str, &str) = (" C-n ", "creates");
    const EDIT: (&str, &str) = (" C-e ", "edits");
    /// What separates them when both are on the row.
    const GAP: &str = " ";

    /// The row a hint spends before its title: the key, the verb, and the quotes the
    /// title is written inside. Measured from the very string that gets rendered, with
    /// the title left out.
    fn chrome((key, verb): (&str, &str)) -> u16 {
        rows::display_width(key) + rows::display_width(&format!("{verb} \"\""))
    }

    let creatable = state.creatable().map(ToString::to_string);
    let editable = state.current().map(|label| label.title.clone());
    if creatable.is_none() && editable.is_none() {
        return None;
    }
    let mut spent = rows::display_width(GAP);
    if creatable.is_some() {
        spent += chrome(CREATE);
    }
    if editable.is_some() {
        spent += chrome(EDIT);
    }
    // What is left over, split between however many titles are on the row -- so a long
    // label cannot push the other key off the end of a line nobody can scroll.
    let titles = u16::from(creatable.is_some()) + u16::from(editable.is_some());
    let room = width.saturating_sub(spent) / titles.max(1);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if let Some(title) = creatable {
        spans.push(Span::styled(CREATE.0.to_string(), theme.accent()));
        spans.push(Span::styled(
            format!("{} \"{}\"", CREATE.1, rows::truncate(&title, room)),
            theme.muted(),
        ));
    }
    if let Some(title) = editable {
        let key = if spans.is_empty() {
            EDIT.0.to_string()
        } else {
            format!("{GAP}{}", EDIT.0)
        };
        spans.push(Span::styled(key, theme.accent()));
        spans.push(Span::styled(
            format!("{} \"{}\"", EDIT.1, rows::truncate(&title, room)),
            theme.muted(),
        ));
    }
    Some(Line::from(spans))
}

/// The label form: every label, with the ones on the task ticked.
fn labels_body(state: &LabelsState, frame: &mut Frame, area: Rect, theme: Theme) {
    let offer = labels_offer(state, theme, area.width);
    // A row of its own, taken off the list rather than added to the box: `lines` is
    // truncated to the height at the end, and a full list would otherwise push the offer
    // off the bottom -- hiding the key precisely when it is being offered.
    let mut lines = vec![input_line(&state.input, theme)];
    let rows = usize::from(area.height).saturating_sub(1 + usize::from(offer.is_some()));
    let first = scrolled_to(state.selected, state.matches.len(), rows);
    for (position, index) in state.matches.iter().enumerate().skip(first) {
        let Some(label) = state.labels.get(*index) else {
            continue;
        };
        let selected = position == state.selected;
        let style = if selected {
            theme.selected(true)
        } else {
            theme.text()
        };
        // A box rather than a colour, so which labels are on the task survives a terminal
        // with no colour at all and a reader who cannot tell two of them apart.
        let tick = if state.is_chosen(label.id) {
            "[x] "
        } else {
            "[ ] "
        };
        let room = area.width.saturating_sub(5);
        lines.push(Line::from(vec![
            Span::styled(format!(" {tick}"), style),
            Span::styled(rows::truncate(&label.title, room), style),
        ]));
    }
    if state.matches.is_empty() {
        lines.push(Line::from(Span::styled(
            if state.labels.is_empty() {
                // The case the deleted "No labels exist yet" toast used to cover. The
                // form opens on an empty pool now, and "nothing matches" over an empty
                // box reads as a broken filter rather than as an invitation.
                " no labels yet — type a name".to_string()
            } else {
                " nothing matches".to_string()
            },
            theme.muted(),
        )));
    }
    // Never below one: on a box clipped to a single row by a short terminal, what the
    // user is typing outranks the offer to create it.
    lines.truncate(
        usize::from(area.height)
            .saturating_sub(usize::from(offer.is_some()))
            .max(1),
    );
    lines.extend(offer);
    frame.render_widget(Paragraph::new(lines), area);
    place_cursor(frame, area, &state.input, 2);
}

/// The question: the names that do not exist, and why it is worth asking.
///
/// The reason is on screen rather than left to the user to remember, because the answer
/// only looks obvious from one side. `y` is the reflex, and what makes it the wrong
/// reflex -- that the pool is shared by every project, so the typo would follow them into
/// all of them -- is not something the task line they just typed says anywhere.
fn confirm_labels_body(state: &ConfirmLabelsState, frame: &mut Frame, area: Rect, theme: Theme) {
    let mut lines: Vec<Line<'static>> = state
        .unknown
        .iter()
        .map(|title| {
            Line::from(Span::styled(
                format!(" {}", rows::truncate(title, area.width.saturating_sub(1))),
                theme.text().add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    // Never below one: on a box clipped by a short terminal, the note that says what is
    // happening outranks the last of a long list of names.
    lines.truncate(usize::from(area.height).saturating_sub(2).max(1));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!(
            " {}",
            rows::truncate(state.note(), area.width.saturating_sub(1))
        ),
        theme.muted(),
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

/// The label form: a title, a colour, and the colour worn as the user types it.
///
/// The chip is the reason the colour is a field rather than a prompt. Six hex digits are
/// not a colour anybody can read, and the label is about to be worn by every task that
/// carries it -- so it is shown in the colour it would have, in the same chip style the
/// list rows use.
fn label_edit_body(state: &LabelEditState, frame: &mut Frame, area: Rect, theme: Theme) {
    /// Room for the wider of the two field names plus its gap.
    const GUTTER: u16 = 10;

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut caret: Option<(u16, u16)> = None;
    for field in [LabelField::Title, LabelField::Colour] {
        let focused = state.field == field;
        let input = state.field(field);
        let room = area.width.saturating_sub(GUTTER + 1);
        // The label carries the focus, the same as the task form: which field has the
        // keyboard is readable without hunting for the caret.
        let name = Span::styled(
            format!(
                " {:<width$}",
                field.label(),
                width = usize::from(GUTTER) - 1
            ),
            if focused {
                theme.pane(true).add_modifier(Modifier::BOLD)
            } else {
                theme.muted()
            },
        );
        if focused {
            let before: String = input.value().chars().take(input.cursor()).collect();
            caret = Some((
                area.x + GUTTER + rows::display_width(&before).min(room),
                area.y + u16::try_from(lines.len()).unwrap_or(0),
            ));
        }
        let mut spans = vec![
            name,
            Span::styled(
                rows::truncate(input.value(), room),
                value_style(theme, focused),
            ),
        ];
        if field == LabelField::Colour && state.colour_is_valid() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!(" {} ", rows::truncate(state.typed_title(), 20)),
                theme.label(state.colour()),
            ));
        }
        lines.push(Line::from(spans));
    }
    // Always a third row, holding the refusal or the rule it broke. A colour that is not
    // six hex digits is refused here rather than queued, because the server's answer
    // arrives minutes later and rolls the *whole* mutation back -- rename included.
    lines.push(match &state.error {
        Some(why) => Line::from(Span::styled(format!(" {why}"), theme.error())),
        None => Line::from(Span::styled(
            " six hex digits, or empty for the interface's own".to_string(),
            theme.muted(),
        )),
    });
    frame.render_widget(Paragraph::new(lines), area);
    // After the widget, so the cursor is not painted over by it.
    if let Some((x, y)) = caret {
        if x < area.right() && y < area.bottom() {
            frame.set_cursor_position((x, y));
        }
    }
}

/// The quick-action menu: one line per configured key.
fn quick_actions_body(state: &QuickActionsState, frame: &mut Frame, area: Rect, theme: Theme) {
    let lines: Vec<Line<'static>> = state
        .rows
        .iter()
        .map(|(key, doc)| {
            Line::from(vec![
                Span::styled(format!(" {key}  "), theme.accent()),
                Span::styled(doc.clone(), theme.text()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// How many lines the description field is given inside the edit form.
const EDIT_DESCRIPTION_LINES: u16 = 4;

/// The edit form: every field of one task, with the focused one marked.
fn edit_body(state: &EditState, frame: &mut Frame, area: Rect, theme: Theme) {
    /// Room for the widest label plus its colon.
    const GUTTER: u16 = 13;

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Where the terminal's own cursor goes. The focused field's label and underline say
    // *which* field has the keyboard; only a real caret says where in it the next
    // character lands, and a multi-line box without one is unreadable.
    let mut caret: Option<(u16, u16)> = None;
    for field in EditField::ALL {
        let focused = state.focus == field;
        let input = state.field(field);
        let width = area.width.saturating_sub(GUTTER + 1);

        // The label carries the focus, so the field being typed into is findable without
        // hunting for a cursor -- the same accent the focused pane uses.
        let label = Span::styled(
            format!(
                " {:<width$}",
                field.label(),
                width = usize::from(GUTTER) - 1
            ),
            if focused {
                theme.pane(true).add_modifier(Modifier::BOLD)
            } else {
                theme.muted()
            },
        );

        if input.is_multiline() {
            let (wrapped, (caret_row, caret_col)) =
                rows::edit_layout(input.value(), width, EDIT_DESCRIPTION_LINES, input.cursor());
            if focused {
                caret = Some((
                    area.x + GUTTER + caret_col,
                    area.y + u16::try_from(lines.len()).unwrap_or(0) + caret_row,
                ));
            }
            for (index, text) in wrapped.iter().enumerate() {
                let gutter = if index == 0 {
                    label.clone()
                } else {
                    Span::raw(" ".repeat(usize::from(GUTTER)))
                };
                lines.push(Line::from(vec![
                    gutter,
                    Span::styled(text.clone(), value_style(theme, focused)),
                ]));
            }
            // Keep the box a constant height whatever the description holds, so the
            // fields below it do not move as the user types.
            for _ in wrapped.len()..usize::from(EDIT_DESCRIPTION_LINES) {
                lines.push(Line::default());
            }
        } else {
            let shown = rows::truncate(input.value(), width);
            if focused {
                let before: String = input.value().chars().take(input.cursor()).collect();
                caret = Some((
                    area.x + GUTTER + rows::display_width(&before).min(width),
                    area.y + u16::try_from(lines.len()).unwrap_or(0),
                ));
            }
            let text = if shown.is_empty() && !focused {
                Span::styled("—".to_string(), theme.muted())
            } else {
                Span::styled(shown, value_style(theme, focused))
            };
            lines.push(Line::from(vec![label, text]));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
    // After the widget, so the cursor is not painted over by it.
    if let Some((x, y)) = caret {
        if x < area.right() && y < area.bottom() {
            frame.set_cursor_position((x, y));
        }
    }
}

/// A form value, marked while its field has the keyboard.
fn value_style(theme: Theme, focused: bool) -> Style {
    if focused {
        theme.text().add_modifier(Modifier::UNDERLINED)
    } else {
        theme.text()
    }
}

/// The quick-add prompt, with what the parser made of it shown as it is typed.
///
/// The syntax is only worth having if it is visible before the task exists. Typing
/// `*urgent !3 +Legal tomorrow` and finding out afterwards what it meant is how magic
/// syntax gets a bad name.
fn add_prompt(model: &Model, input: &TextInput, frame: &mut Frame) {
    let theme = model.theme;
    let area = model.frames().status;
    if area.height == 0 {
        return;
    }

    let parsed = tui_do_core::quickadd::parse(input.value(), &model.now);
    let mut summary: Vec<String> = Vec::new();
    if let Some(project) = &parsed.project {
        summary.push(format!("+{project}"));
    }
    for label in &parsed.labels {
        summary.push(format!("*{label}"));
    }
    if let Some(priority) = parsed.priority {
        summary.push(format!("P{priority}"));
    }
    if let Some(due) = parsed.due_date {
        summary.push(format!("due {}", rows::relative_date(Some(due), model.now)));
    }
    if parsed.repeat.is_some() {
        summary.push("repeats".to_string());
    }

    let right = if input.value().is_empty() {
        // Nothing typed yet is exactly when the syntax is worth showing. There is no
        // order to remember -- the parser takes these anywhere in the line -- so the
        // legend is the whole of what there is to learn.
        vec![Span::styled(
            "*label  +project  !1-5  @user  a date   Esc:cancel",
            theme.muted(),
        )]
    } else if summary.is_empty() {
        vec![Span::styled("Enter:add  Esc:cancel", theme.muted())]
    } else {
        vec![
            Span::styled(summary.join(" · "), theme.accent()),
            Span::styled("  Enter:add  Esc:cancel", theme.muted()),
        ]
    };
    let left = vec![
        Span::styled("+ ", theme.accent()),
        Span::styled(input.value().to_string(), theme.text()),
    ];
    let padding = usize::from(
        area.width
            .saturating_sub(line_width(&left) + line_width(&right)),
    );
    let mut spans = fit(left, area.width.saturating_sub(line_width(&right) + 1));
    spans.push(Span::raw(" ".repeat(padding)));
    spans.extend(right);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    place_cursor(frame, area, input, 2);
}

/// The search prompt, along the status line, with a live count of what matches.
fn search_prompt(model: &Model, state: &SearchState, frame: &mut Frame) {
    let theme = model.theme;
    let area = model.frames().status;
    if area.height == 0 {
        return;
    }

    let left = vec![
        Span::styled("/", theme.accent()),
        Span::styled(state.input.value().to_string(), theme.text()),
    ];
    let count = if model.data.loading {
        String::new()
    } else if state.input.value().is_empty() {
        format!("{} tasks", model.data.tasks.len())
    } else if model.data.tasks.len() == 1 {
        "1 match".to_string()
    } else {
        format!("{} matches", model.data.tasks.len())
    };
    let right = vec![Span::styled(
        format!("{count}  Enter:keep  Esc:cancel"),
        theme.muted(),
    )];
    let padding = usize::from(
        area.width
            .saturating_sub(line_width(&left) + line_width(&right)),
    );

    let mut spans = fit(left, area.width.saturating_sub(line_width(&right) + 1));
    spans.push(Span::raw(" ".repeat(padding)));
    spans.extend(right);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    place_cursor(frame, area, &state.input, 1);
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
    let rows = usize::from(area.height).saturating_sub(1);
    let first = scrolled_to(picker.selected, picker.matches.len(), rows);
    for (position, index) in picker.matches.iter().enumerate().skip(first) {
        let Some(candidate) = picker.candidates.get(*index) else {
            continue;
        };
        let selected = position == picker.selected;
        let style = if selected {
            theme.selected(true)
        } else {
            theme.text()
        };
        // The hint is right-aligned, so the palette reads as "what it does … which key",
        // and using a command by name teaches the binding for next time.
        let hint_width = rows::display_width(&candidate.hint);
        // +3 rather than +2: one for the leading space, one for the gap, and one so the
        // key does not sit against the border.
        let room = area.width.saturating_sub(hint_width + 3);
        let title = rows::truncate(&candidate.title, room);
        let gap = usize::from(room.saturating_sub(rows::display_width(&title))) + 1;
        let mut spans = vec![
            Span::styled(format!(" {title}"), style),
            Span::styled(" ".repeat(gap), style),
        ];
        if hint_width > 0 {
            spans.push(Span::styled(
                candidate.hint.clone(),
                if selected { style } else { theme.muted() },
            ));
        }
        lines.push(Line::from(spans));
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
