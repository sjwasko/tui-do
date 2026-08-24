//! Where the panes go.
//!
//! One function computes every rectangle from the terminal size and which optional panes
//! are showing. `update` uses it to know how many rows a page is; `view` uses it to draw.
//! Two answers to that question would drift apart the first time a border moved.

use ratatui::layout::Rect;

/// Below this width the sidebar hides itself when it is on `Auto`.
pub const SIDEBAR_AUTO_MIN: u16 = 100;

/// Below this width the preview hides itself when it is on `Auto`.
pub const PREVIEW_AUTO_MIN: u16 = 120;

/// Below this width the sidebar cannot show at all, even pinned.
///
/// A pinned pane still yields when there is genuinely no room: honouring the pin at 50
/// columns would leave a task list too narrow to read a title in.
pub const SIDEBAR_HARD_MIN: u16 = 60;

/// Below this width the preview cannot show at all, even pinned.
pub const PREVIEW_HARD_MIN: u16 = 70;

/// The breadcrumb-and-tabs strip, plus the rule under it.
pub const HEADER_HEIGHT: u16 = 2;

/// The status line.
pub const STATUS_HEIGHT: u16 = 1;

/// The column headings inside the task list.
pub const LIST_HEADING_HEIGHT: u16 = 1;

/// Where everything is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frames {
    /// Breadcrumb, view tabs and the sync indicator.
    pub header: Rect,
    /// The project tree, when showing.
    pub sidebar: Option<Rect>,
    /// The task list, which is always present.
    pub list: Rect,
    /// The task preview, when showing.
    pub preview: Option<Rect>,
    /// Counts, queued changes and the key hints.
    pub status: Rect,
}

impl Frames {
    /// How many task rows fit, assuming one line each.
    ///
    /// Wrapped rows are taller, so this is the floor rather than the exact count. It is
    /// what a page motion moves by, which is why an approximation is acceptable here and
    /// would not be in the renderer.
    #[must_use]
    pub const fn list_rows(&self) -> usize {
        self.list.height.saturating_sub(LIST_HEADING_HEIGHT) as usize
    }
}

/// The width the sidebar takes at this terminal width.
#[must_use]
pub fn sidebar_width(total: u16) -> u16 {
    (total / 5).clamp(20, 32)
}

/// The width the preview takes at this terminal width.
#[must_use]
pub fn preview_width(total: u16) -> u16 {
    (total / 3).clamp(32, 60)
}

/// Lay out a terminal of `width` by `height`.
///
/// The optional panes are dropped in order — preview first, then sidebar — if the
/// remaining width would leave the list unusable, so a very narrow terminal degrades to
/// the list rather than to three unreadable columns.
#[must_use]
pub fn frames(width: u16, height: u16, sidebar: bool, preview: bool) -> Frames {
    /// The narrowest task list worth drawing.
    const LIST_MIN: u16 = 30;

    let header = Rect::new(0, 0, width, HEADER_HEIGHT.min(height));
    let body_top = header.height;
    let body_height = height
        .saturating_sub(HEADER_HEIGHT)
        .saturating_sub(STATUS_HEIGHT);
    let status = Rect::new(0, body_top + body_height, width, STATUS_HEIGHT.min(height));

    let mut left = 0;
    let mut remaining = width;

    let sidebar_rect = if sidebar && remaining.saturating_sub(sidebar_width(width)) >= LIST_MIN {
        let w = sidebar_width(width);
        let rect = Rect::new(left, body_top, w, body_height);
        left += w;
        remaining -= w;
        Some(rect)
    } else {
        None
    };

    let preview_rect = if preview && remaining.saturating_sub(preview_width(width)) >= LIST_MIN {
        let w = preview_width(width);
        remaining -= w;
        Some(Rect::new(left + remaining, body_top, w, body_height))
    } else {
        None
    };

    Frames {
        header,
        sidebar: sidebar_rect,
        list: Rect::new(left, body_top, remaining, body_height),
        preview: preview_rect,
        status,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_panes_tile_the_body_without_overlapping() {
        let frames = frames(160, 50, true, true);
        let sidebar = frames.sidebar.unwrap();
        let preview = frames.preview.unwrap();
        assert_eq!(sidebar.x, 0);
        assert_eq!(sidebar.right(), frames.list.x);
        assert_eq!(frames.list.right(), preview.x);
        assert_eq!(preview.right(), 160);
        assert_eq!(
            frames.header.height + frames.list.height + frames.status.height,
            50
        );
    }

    #[test]
    fn a_narrow_terminal_keeps_the_list_and_drops_the_rest() {
        // Asked for both panes, but a sidebar here would leave the list unreadable. This
        // is the floor under `PaneState`'s own hard minimum, not a duplicate of it: this
        // one holds however the booleans were arrived at.
        let frames = frames(45, 20, true, true);
        assert!(frames.sidebar.is_none());
        assert!(frames.preview.is_none());
        assert_eq!(frames.list.width, 45);
    }

    #[test]
    fn the_preview_yields_before_the_sidebar_does() {
        let frames = frames(75, 30, true, true);
        assert!(frames.sidebar.is_some());
        assert!(frames.preview.is_none());
    }

    #[test]
    fn a_page_is_the_list_minus_its_headings() {
        let frames = frames(120, 40, false, false);
        assert_eq!(frames.list_rows(), 40 - 2 - 1 - 1);
    }
}
