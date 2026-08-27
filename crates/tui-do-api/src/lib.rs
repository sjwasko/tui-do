//! Typed client for the go-vikunja REST API.
//!
//! Every endpoint this crate calls is verified against `spec/vikunja.json`, the OpenAPI
//! document served by a live Vikunja instance at `/api/v1/docs.json`. See the conformance
//! test in `tests/`: it fails the build when a path template we construct is absent from
//! the spec, which is how the upstream `/tasks/all` -> `/tasks` rename would have been
//! caught before it broke anything.

#![doc(html_no_source)]

pub mod auth;
pub mod client;
pub mod endpoints;
pub mod error;
pub mod models;
pub mod pagination;
pub mod query;
pub mod secret;

pub use auth::{AuthKind, Credentials};
pub use client::{Client, ClientBuilder};
pub use error::{ApiError, Result};
pub use pagination::{Page, PageInfo, Pager};
pub use query::{Expand, Order, TaskQuery};
pub use secret::Secret;
