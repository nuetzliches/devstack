//! How this copy of devstack came onto the machine, which says who updates
//! and removes it: a build from source (nobody), `cargo install` (cargo),
//! winget (winget), or the installers (devstack itself, `install_dir`).

use std::path::{Path, PathBuf};

/// A cargo build (`.../target/...`) is not where the user keeps devstack; a
/// registration pointing there breaks with the next `cargo clean`.
pub fn is_dev_build(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "target")
}

/// `cargo install` puts binaries into `$CARGO_HOME/bin` (`~/.cargo/bin`);
/// replacing one with a release build would surprise its next `cargo install`.
pub fn is_cargo_install(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    if let Some(home) = std::env::var_os("CARGO_HOME")
        && dir == Path::new(&home).join("bin")
    {
        return true;
    }
    let mut parts = dir.components().rev();
    parts.next().is_some_and(|c| c.as_os_str() == "bin")
        && parts.next().is_some_and(|c| c.as_os_str() == ".cargo")
}

/// The package winget installs devstack as (`packaging/winget/`).
pub const WINGET_ID: &str = "nuetzliches.devstack";

/// winget keeps a portable package in `...\WinGet\Packages\<id>_<source>\`
/// and puts a link to it in `...\WinGet\Links\` (per user under
/// `%LOCALAPPDATA%\Microsoft`, for the machine under `%ProgramFiles%`).
/// Replacing that binary behind winget's back leaves winget with the old
/// version on record.
pub fn is_winget_install(exe: &Path) -> bool {
    let path = exe
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    path.contains(r"\winget\packages\") || path.contains(r"\winget\links\")
}

/// Where the installers put devstack; a binary elsewhere (a cargo build) is
/// not theirs.
pub fn install_dir() -> Option<PathBuf> {
    crate::os::install_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cargo_build_is_recognised() {
        assert!(is_dev_build(Path::new(
            "/src/devstack/target/release/devstack"
        )));
        assert!(!is_dev_build(Path::new("/home/u/.local/bin/devstack")));
    }

    #[test]
    fn a_cargo_install_is_recognised() {
        let home = if cfg!(windows) {
            r"C:\Users\u\.cargo\bin\devstack.exe"
        } else {
            "/home/u/.cargo/bin/devstack"
        };
        assert!(is_cargo_install(Path::new(home)));
        assert!(!is_cargo_install(Path::new("/home/u/.local/bin/devstack")));
    }

    #[test]
    fn a_winget_install_is_recognised() {
        for winget in [
            r"C:\Users\u\AppData\Local\Microsoft\WinGet\Packages\nuetzliches.devstack_Microsoft.Winget.Source_8wekyb3d8bbwe\devstack.exe",
            r"C:\Users\u\AppData\Local\Microsoft\WinGet\Links\devstack.exe",
            r"C:\Program Files\WinGet\Packages\nuetzliches.devstack_Microsoft.Winget.Source_8wekyb3d8bbwe\devstack.exe",
        ] {
            assert!(is_winget_install(Path::new(winget)), "{winget}");
        }
        for not in [
            r"C:\Users\u\AppData\Local\Programs\devstack\devstack.exe",
            r"C:\src\winget-packages\devstack.exe",
            "/home/u/.local/bin/devstack",
        ] {
            assert!(!is_winget_install(Path::new(not)), "{not}");
        }
    }
}
