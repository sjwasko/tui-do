//! The terminal UI layer: `Model`, `Msg`, `update`, `view`.
//!
//! # The one rule
//!
//! **Nothing in this crate performs I/O.** [`update`] is a pure, synchronous function
//! from `(&mut Model, Msg)` to a list of [`Effect`]s, and rendering only reads the model.
//! Network calls, database writes and file access happen in the effect runtime, which
//! lives in the binary crate and communicates over channels.
//!
//! `tui-do-ui` names none of `reqwest`, `rusqlite` or `tokio` as a dependency, so none of
//! them can be reached by name from this crate. Be precise about how much that buys: all
//! three arrive transitively through `tui-do-core`, and `tui_do_core::Store` is a public
//! re-export that `update` could call and block the render thread with. What actually
//! holds the rule is that [`update`] is a synchronous function returning [`Effect`]s, and
//! that nothing here calls a store. `a_pure_ui_names_no_io` asserts the second half.
//!
//! That is deliberate. The project this replaces awaited network calls while holding a
//! lock on its application state, which froze the terminal for the duration of every slow
//! request. Do not add an I/O dependency here, and do not reach for `Store`.
//!
//! # How a keystroke becomes a screen
//!
//! ```text
//! terminal event ─▶ Msg::Key ─▶ update ─▶ Effect::LoadTasks ─▶ (runtime, off-thread)
//!                                  │                                    │
//!                                  ▼                                    ▼
//!                              Model changed                     Msg::TasksLoaded
//!                                  │                                    │
//!                                  ▼                                    ▼
//!                               render ◀───────────────────────────── update
//! ```
//!
//! Two invariants hold that loop together. Every task query carries a
//! [`query::QueryId`], and an answer that is no longer current is dropped rather than
//! rendered — otherwise holding `j` down the sidebar paints whichever project's query
//! happened to finish last. And the list's selection is a [`tui_do_core::models::TaskId`],
//! never a row index, so a reload that reorders or drops rows cannot silently move the
//! cursor onto a different task.

pub mod effect;
pub mod geometry;
pub mod keymap;
pub mod markdown;
pub mod modal;
pub mod model;
pub mod msg;
pub mod query;
pub mod rows;
pub mod sidebar;
pub mod theme;
pub mod update;
pub mod urls;
pub mod view;

pub use effect::Effect;
pub use keymap::{Action, Binding, Key, KEYMAP};
pub use modal::{Modal, ModalView};
pub use model::{Focus, Model, PaneState, Screen};
pub use msg::Msg;
pub use query::{Query, QueryId, Scope};
pub use update::{past_due_note, quickadd_task, update, QuickAdd};
pub use view::view;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod purity {
    /// The render loop never awaits I/O, checked rather than assumed.
    ///
    /// The crate's stated enforcement was the dependency ban, and the ban is weaker than
    /// it reads: `rusqlite`, `reqwest` and `tokio` all arrive transitively through
    /// `tui-do-core`, and `tui_do_core::Store` is a public re-export that any function
    /// here could call. `use rusqlite::…` still will not compile, but `Store::open` would
    /// -- and it would block the thread that draws the screen, which is the exact failure
    /// this architecture exists to prevent.
    ///
    /// So the rule is asserted against the source instead. Cheap now; archaeology later.
    #[test]
    fn a_pure_ui_names_no_io() {
        const BANNED: &[&str] = &[
            "rusqlite",
            "reqwest",
            "tokio",
            "Store",
            "spawn_blocking",
            ".await",
        ];

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offences = Vec::new();
        let mut stack = vec![dir];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).expect("src is readable") {
                let entry = entry.expect("a readable directory entry").path();
                if entry.is_dir() {
                    stack.push(entry);
                    continue;
                }
                if entry.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&entry).expect("a readable source file");
                // This module names every banned word in order to ban it.
                let source = source.split("mod purity {").next().unwrap_or("");
                for (number, line) in source.lines().enumerate() {
                    // Prose may name what the code may not: every one of these words
                    // appears in a comment explaining why it is banned.
                    let code = line.split("//").next().unwrap_or("");
                    for banned in BANNED {
                        // Whole-word, so `Msg::StoreFailed` -- a message *about* a store
                        // failure, carrying a `String` -- is not read as touching one.
                        if code.match_indices(banned).any(|(at, _)| {
                            code[at + banned.len()..]
                                .chars()
                                .next()
                                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                        }) {
                            offences.push(format!(
                                "{}:{}: {}",
                                entry.display(),
                                number + 1,
                                line.trim()
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            offences.is_empty(),
            "tui-do-ui reached for I/O:\n{}",
            offences.join("\n")
        );
    }
}
