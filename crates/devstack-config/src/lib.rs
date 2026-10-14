//! The stack file (`devstack.toml`): model, strict loading, validation and
//! interpolation. See ADR-0017 and plan §4.

pub mod interpolate;
pub mod model;
pub mod validate;

pub use model::Stack;
pub use validate::Issue;

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Syntax, type and unknown-key errors, with line and column from the TOML parser.
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{path}: {} problem(s)\n{}", .issues.len(), .issues.iter().map(|i| format!("  - {i}")).collect::<Vec<_>>().join("\n"))]
    Invalid { path: PathBuf, issues: Vec<Issue> },
    /// `[stack].requires` names a newer devstack than this one.
    #[error(
        "{path} needs devstack {required} or newer, this is {have}. Update devstack (README, \"Install\")."
    )]
    TooOld {
        path: PathBuf,
        required: String,
        have: String,
    },
}

/// Version of this devstack.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `"1.2.3"`, `"v1.2"`, `"1"` as numbers; `None` when malformed.
pub fn version_triple(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// `[stack].requires`, read before the strict parse: a stack file written for
/// a newer devstack fails with "update devstack", not with an unknown key.
fn check_requires(text: &str, path: &Path) -> Result<(), LoadError> {
    let Ok(value) = toml::from_str::<toml::Table>(text) else {
        return Ok(()); // the strict parse reports the syntax error
    };
    let Some(required) = value
        .get("stack")
        .and_then(|s| s.get("requires"))
        .and_then(|r| r.as_str())
    else {
        return Ok(());
    };
    match (version_triple(required), version_triple(VERSION)) {
        (Some(r), Some(h)) if h < r => Err(LoadError::TooOld {
            path: path.to_path_buf(),
            required: required.to_string(),
            have: VERSION.to_string(),
        }),
        _ => Ok(()), // malformed: validation reports it
    }
}

/// Parse and validate a stack file given as text. `path` only labels errors.
pub fn from_str(text: &str, path: &Path) -> Result<Stack, LoadError> {
    check_requires(text, path)?;
    let stack: Stack = toml::from_str(text).map_err(|e| LoadError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let issues = validate::validate(&stack);
    if issues.is_empty() {
        Ok(stack)
    } else {
        Err(LoadError::Invalid {
            path: path.to_path_buf(),
            issues,
        })
    }
}

/// Read, parse and validate a stack file.
pub fn load(path: &Path) -> Result<Stack, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    from_str(&text, path)
}

/// JSON Schema of the stack file, for editors (taplo, VS Code).
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Stack)).expect("schema serialises")
}
