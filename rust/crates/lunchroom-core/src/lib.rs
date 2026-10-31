//! Lunchroom's core: the decisions, words and stores that do not depend on any one harness.
//!
//! Governing spec: `docs/spec/lunchroom-spec.md`.

pub mod approvals;
pub mod cards;
pub mod classify;
pub mod fs;
pub mod identity;
pub mod jcs;
pub mod loops;
pub mod mail;
pub mod passphrase;
pub mod paths;
pub mod policy;
pub mod secrets;
pub mod shell;
pub mod store;
pub mod vocab;

/// The build folder this test runs from (`<target>/release/deps/<test>`), for scratch files.
#[cfg(test)]
fn test_target() -> std::path::PathBuf {
    let test = std::env::current_exe().unwrap_or_default();
    test.ancestors()
        .nth(3)
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf()
}
