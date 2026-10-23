//! The stack file (`devstack.toml`): model, strict loading, validation and
//! interpolation. See ADR-0017 and plan §4.

pub mod compat;
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
    /// `[stack].requires` names a newer devstack than this one. The CLI
    /// recognises this wording in a daemon's answer (an older daemon's too)
    /// and says to restart the daemon when it is new enough itself: keep it.
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

/// The first devstack that knows `[stack] reviewed`: a file that sets it
/// wants `requires` at least this.
pub const REVIEWED_SINCE: &str = "0.20.0";

/// The first devstack that knows `[compose.<group>] announced_ports`
/// (ADR-0057): a file that sets it wants `requires` at least this.
pub const ANNOUNCED_SINCE: &str = "0.20.0";

/// The first devstack that knows `[profiles.<name>] skip` (ADR-0058): a
/// file that sets it wants `requires` at least this.
pub const SKIP_SINCE: &str = "0.21.0";

/// The first devstack that knows `[services.<name>] stop` (ADR-0059): a
/// file that sets it wants `requires` at least this.
pub const STOP_SINCE: &str = "0.21.0";

/// `"1.2.3"`, `"v1.2"`, `"1"` as numbers; `None` when malformed. A
/// pre-release or build suffix (`1.2.3-rc1`, `1.2.3+abc`) is ignored here;
/// [`version_order`] takes it into account.
pub fn version_triple(v: &str) -> Option<(u64, u64, u64)> {
    version_order(v).map(|(t, _)| t)
}

/// A version as a sortable key: the triple, then `true` for a release and
/// `false` for a pre-release, which comes before the release of the same
/// number (`0.9.8-rc1` < `0.9.8`, as in semver). `None` when malformed.
pub fn version_order(v: &str) -> Option<((u64, u64, u64), bool)> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split_once('+').map_or(v, |(core, _)| core);
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => (core, true),
        Some(_) => return None,
        None => (v, false),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    Some(((major, minor, patch), !pre))
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
    match (version_order(required), version_order(VERSION)) {
        (Some(r), Some(h)) if h < r => Err(LoadError::TooOld {
            path: path.to_path_buf(),
            required: required.to_string(),
            have: VERSION.to_string(),
        }),
        _ => Ok(()), // malformed: validation reports it
    }
}

/// `version`, read before the strict parse (ADR-0023): a file of another
/// schema version fails with "unsupported version", not with the first key
/// this version does not know.
fn check_version(text: &str, path: &Path) -> Result<(), LoadError> {
    let Ok(value) = toml::from_str::<toml::Table>(text) else {
        return Ok(());
    };
    match value.get("version").and_then(|v| v.as_integer()) {
        Some(v) if v != i64::from(model::SCHEMA_VERSION) => Err(LoadError::Invalid {
            path: path.to_path_buf(),
            issues: vec![Issue {
                path: "version".into(),
                message: validate::unsupported_version(v),
            }],
        }),
        _ => Ok(()),
    }
}

/// Parse and validate a stack file given as text. `path` only labels errors.
pub fn from_str(text: &str, path: &Path) -> Result<Stack, LoadError> {
    check_requires(text, path)?;
    check_version(text, path)?;
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
