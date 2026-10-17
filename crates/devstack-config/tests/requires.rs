//! `[stack].requires`: an old devstack asks to be updated.

use devstack_config::{LoadError, VERSION, from_str, version_triple};
use std::path::Path;

fn file(requires: &str, extra: &str) -> String {
    format!("version = 1\n[stack]\nname = \"t\"\nrequires = \"{requires}\"\n{extra}")
}

#[test]
fn versions_parse() {
    assert_eq!(version_triple("1.2.3"), Some((1, 2, 3)));
    assert_eq!(version_triple("v0.4"), Some((0, 4, 0)));
    assert_eq!(version_triple("2"), Some((2, 0, 0)));
    assert_eq!(version_triple("1.2.x"), None);
    assert_eq!(version_triple("1.2.3.4"), None);
}

#[test]
fn a_file_for_this_version_loads() {
    from_str(&file(VERSION, ""), Path::new("devstack.toml")).unwrap();
}

#[test]
fn a_newer_file_asks_for_an_update_before_complaining_about_keys() {
    // A key this version does not know, as a newer file would have.
    let text = file("999.0.0", "[stack.future]\nkey = 1\n");
    match from_str(&text, Path::new("devstack.toml")) {
        Err(LoadError::TooOld { required, have, .. }) => {
            assert_eq!(required, "999.0.0");
            assert_eq!(have, VERSION);
        }
        other => panic!("expected TooOld, got {other:?}"),
    }
}

#[test]
fn a_malformed_requirement_is_a_validation_error() {
    let err = from_str(&file("soon", ""), Path::new("devstack.toml")).unwrap_err();
    assert!(err.to_string().contains("stack.requires"), "{err}");
}

#[test]
fn a_pre_release_comes_before_its_release() {
    use devstack_config::version_order;
    assert_eq!(version_triple("0.9.8-rc1"), Some((0, 9, 8)));
    assert!(version_order("0.9.8-rc1") < version_order("0.9.8"));
    assert!(version_order("0.9.8-rc1") > version_order("0.9.7"));
    assert_eq!(version_order("1.0.0+build"), version_order("1.0.0"));
    assert_eq!(version_order("1.0.0-"), None);
}
