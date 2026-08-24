//! Domain model, local-first store, sync engine, quick-add parser and configuration.
//!
//! This crate owns the answer to "what is true right now": the SQLite store is the source
//! of truth the UI reads from, and the sync engine reconciles it with the server in the
//! background. Nothing here renders, and nothing here blocks a frame.

pub mod config;
pub mod error;
pub mod quickadd;

pub use config::Config;
pub use error::{CoreError, Result};
