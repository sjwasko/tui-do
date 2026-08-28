//! Domain model, local-first store, sync engine, quick-add parser and configuration.
//!
//! This crate owns the answer to "what is true right now": the SQLite store is the source
//! of truth the UI reads from, and the sync engine reconciles it with the server in the
//! background. Nothing here renders, and nothing here blocks a frame.

pub mod config;
pub mod error;
pub mod quickadd;
pub mod store;
pub mod sync;

/// The wire models, re-exported.
///
/// `tui-do-ui` names `Task`, `Project` and `Label` constantly but must not depend on
/// `tui-do-api` directly, because that crate carries `reqwest` and the UI layer's
/// dependency list *is* the enforcement mechanism for "the render loop never awaits I/O".
/// Reaching them through here keeps both facts true.
pub use tui_do_api::models;

pub use config::Config;
pub use error::{CoreError, Result};
pub use store::Store;
pub use sync::{Reach, Sync, SyncEvent, SyncReport};
