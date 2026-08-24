//! Conformance of this crate against `spec/vikunja.json`.
//!
//! The spec is the OpenAPI document served by a live Vikunja at `/api/v1/docs.json`,
//! captured by `cargo xtask fetch-spec`. These tests are what make it authoritative
//! rather than decorative:
//!
//! - every path template in [`criax_api::endpoints`] must exist in the spec, so an
//!   endpoint that upstream renames becomes a failing test instead of a runtime 404;
//! - every field on our model structs must exist in the corresponding spec definition,
//!   so a renamed or removed field is caught at build time rather than silently
//!   deserializing to a default.
//!
//! Together these cover what generating the models from the spec would have, and
//! additionally catch fields we have stopped modelling.

// Assertions and fixture loading legitimately panic; that is how a test reports failure.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::panic
)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use criax_api::endpoints;
use criax_api::models::{
    Bucket, Label, Project, ProjectView, Task, TaskAttachment, TaskComment, TaskReminder, User,
};
use serde::Serialize;
use serde_json::Value;

/// Load the captured spec.
fn spec() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/vikunja.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "could not read {} ({e}). Run `cargo xtask fetch-spec`.",
            path.display()
        )
    });
    serde_json::from_str(&raw).expect("spec is not valid JSON")
}

/// The JSON keys a model produces on the wire, taken from a default instance.
///
/// Reading the keys off a serialized value rather than a macro keeps the check honest:
/// it sees exactly what the client would send and expects back, including any renames
/// applied by serde attributes.
fn wire_fields<T: Serialize + Default>() -> BTreeSet<String> {
    let value = serde_json::to_value(T::default()).expect("model should serialize");
    match value {
        Value::Object(map) => map.keys().cloned().collect(),
        other => panic!("expected a JSON object, got {other:?}"),
    }
}

/// The property names a spec definition declares.
fn spec_fields(spec: &Value, definition: &str) -> BTreeSet<String> {
    spec.pointer(&format!("/definitions/{definition}/properties"))
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("spec has no definition {definition}"))
        .keys()
        .cloned()
        .collect()
}

#[test]
fn every_endpoint_exists_in_the_spec() {
    let spec = spec();
    let paths: BTreeSet<&str> = spec["paths"]
        .as_object()
        .expect("spec has no paths")
        .keys()
        .map(String::as_str)
        .collect();

    let missing: Vec<&str> = endpoints::ALL
        .iter()
        .copied()
        .filter(|p| !paths.contains(p))
        .collect();

    assert!(
        missing.is_empty(),
        "these endpoints are not served by the Vikunja the spec was captured from: {missing:#?}\n\
         Either the server changed (refresh with `cargo xtask fetch-spec` and read the diff) \
         or the template's parameter spelling does not match the spec's."
    );
}

#[test]
fn the_endpoint_list_has_no_duplicates() {
    let unique: BTreeSet<&str> = endpoints::ALL.iter().copied().collect();
    assert_eq!(
        unique.len(),
        endpoints::ALL.len(),
        "endpoints::ALL contains a duplicate"
    );
}

#[test]
fn the_renamed_tasks_endpoint_is_not_resurrected() {
    // The specific regression criax exists to prevent: cria hardcoded /tasks/all, which
    // upstream renamed to /tasks. If a future spec refresh brings it back, or someone
    // reintroduces the old spelling, this fails loudly.
    let spec = spec();
    let paths = spec["paths"].as_object().unwrap();
    assert!(
        paths.contains_key("/tasks"),
        "/tasks is missing from the spec"
    );
    assert!(
        !endpoints::ALL.contains(&"/tasks/all"),
        "/tasks/all is the pre-rename spelling and must not be called"
    );
}

/// Assert a model's fields are all declared by its spec definition.
fn check_model<T: Serialize + Default>(spec: &Value, definition: &str) {
    let ours = wire_fields::<T>();
    let theirs = spec_fields(spec, definition);

    let unknown: Vec<&String> = ours.difference(&theirs).collect();
    assert!(
        unknown.is_empty(),
        "{definition}: these fields are sent or expected by criax but are not in the spec: \
         {unknown:#?}\nThe server will ignore them on write and never send them on read."
    );

    // Not a failure: we deliberately model a subset. Printed so the gap is visible when
    // a test run is inspected, and so a newly added upstream field gets noticed.
    let unmodelled: Vec<&String> = theirs.difference(&ours).collect();
    if !unmodelled.is_empty() {
        println!("{definition}: not modelled (fine, but worth knowing): {unmodelled:?}");
    }
}

#[test]
fn model_fields_exist_in_the_spec() {
    let spec = spec();
    check_model::<Task>(&spec, "models.Task");
    check_model::<Project>(&spec, "models.Project");
    check_model::<ProjectView>(&spec, "models.ProjectView");
    check_model::<Label>(&spec, "models.Label");
    check_model::<User>(&spec, "user.User");
    check_model::<TaskComment>(&spec, "models.TaskComment");
    check_model::<TaskAttachment>(&spec, "models.TaskAttachment");
    check_model::<TaskReminder>(&spec, "models.TaskReminder");
    check_model::<Bucket>(&spec, "models.Bucket");
}

#[test]
fn the_spec_is_the_version_we_target() {
    // A spec refreshed from a different server version should be a deliberate act, not
    // something that slips in. Bump this when the dev instance is upgraded.
    let spec = spec();
    assert_eq!(
        spec.pointer("/info/version").and_then(Value::as_str),
        Some("v2.5.0"),
        "spec/vikunja.json was captured from a different Vikunja version"
    );
}

#[test]
fn pagination_headers_are_documented_as_expected() {
    // The pagination contract criax relies on. cria ignored these headers, requested
    // per_page=10000, was silently capped at 50, and dropped every task past the first
    // page. If upstream ever renames them, this catches it.
    let spec = spec();
    let description = spec
        .pointer("/info/description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for header in ["x-pagination-total-pages", "x-pagination-result-count"] {
        assert!(
            description.contains(header),
            "the spec no longer documents {header}; pagination handling needs review"
        );
    }
}
