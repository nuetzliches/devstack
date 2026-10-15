//! Stack files as earlier devstack versions wrote them must keep loading
//! (ADR-0023). Each file in `tests/compat/` is a frozen copy, named
//! `v<schema>-<devstack version>-<what>.toml`; never edit one, add a new copy
//! when the format grows. A failure here means a change breaks existing files:
//! either make it compatible, or (once devstack has users outside its
//! maintainers) ship a migration.

use std::path::Path;

#[test]
fn every_frozen_stack_file_still_loads() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let path = e.unwrap().path();
        if path.extension().is_none_or(|x| x != "toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        if let Err(e) = devstack_config::from_str(&text, &path) {
            panic!("{} no longer loads:\n{e}", path.display());
        }
        n += 1;
    }
    assert!(n >= 2, "frozen stack files missing in {}", dir.display());
}
