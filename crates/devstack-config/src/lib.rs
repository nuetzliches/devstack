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
}

/// Parse and validate a stack file given as text. `path` only labels errors.
pub fn from_str(text: &str, path: &Path) -> Result<Stack, LoadError> {
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
