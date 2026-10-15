//! Compatibility of stack files across devstack versions (ADR-0023).
//!
//! A key that is renamed or removed stays readable for a while: the model keeps
//! accepting it (`#[serde(alias = "...")]`, or an optional field that is no
//! longer used), and it is listed in [`DEPRECATED`], so `devstack check` and
//! `devstack doctor` say what replaces it. The table is empty until the first
//! such change.

use crate::validate::Issue;

/// A key that still works but should be replaced.
pub struct Deprecation {
    /// Dotted path; `*` matches any one key, e.g. `services.*.restart`.
    pub path: &'static str,
    /// What to write instead.
    pub replacement: &'static str,
    /// The devstack version that deprecated it.
    pub since: &'static str,
}

pub const DEPRECATED: &[Deprecation] = &[];

/// Warnings for deprecated keys in a stack file's text (never errors).
pub fn warnings(text: &str) -> Vec<Issue> {
    warnings_with(text, DEPRECATED)
}

fn warnings_with(text: &str, table: &[Deprecation]) -> Vec<Issue> {
    let Ok(doc) = text.parse::<toml::Table>() else {
        return Vec::new(); // the strict parse reports it
    };
    let mut out = Vec::new();
    for d in table {
        let parts: Vec<&str> = d.path.split('.').collect();
        collect(
            &toml::Value::Table(doc.clone()),
            &parts,
            String::new(),
            &mut |path| {
                out.push(Issue {
                    path,
                    message: format!(
                        "deprecated since devstack {}; use {}",
                        d.since, d.replacement
                    ),
                });
            },
        );
    }
    out
}

fn collect(v: &toml::Value, parts: &[&str], prefix: String, hit: &mut dyn FnMut(String)) {
    let Some((first, rest)) = parts.split_first() else {
        hit(prefix);
        return;
    };
    let Some(t) = v.as_table() else { return };
    let join = |k: &str| {
        if prefix.is_empty() {
            k.to_string()
        } else {
            format!("{prefix}.{k}")
        }
    };
    if *first == "*" {
        for (k, child) in t {
            collect(child, rest, join(k), hit);
        }
    } else if let Some(child) = t.get(*first) {
        collect(child, rest, join(first), hit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deprecated_key_is_found_with_its_path() {
        let table = [Deprecation {
            path: "services.*.restart",
            replacement: "`watch`",
            since: "9.9.0",
        }];
        let text = "version = 1\n[services.api]\nrun = \"x\"\nrestart = [\"src/**\"]\n[services.web]\nrun = \"y\"\n";
        let w = warnings_with(text, &table);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].path, "services.api.restart");
        assert!(w[0].message.contains("9.9.0") && w[0].message.contains("`watch`"));
    }

    #[test]
    fn nothing_is_deprecated_yet() {
        assert!(warnings("version = 1\n[stack]\nname = \"x\"\n").is_empty());
    }
}
