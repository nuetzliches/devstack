//! How this copy of devstack came onto the machine, which says who updates
//! and removes it: a build from source (nobody), `cargo install` (cargo),
//! winget (winget), Homebrew (`brew`, on macOS and Linux), or the installers
//! (devstack itself, `install_dir`).

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

/// The formula Homebrew installs devstack from (the tap
/// `nuetzliches/homebrew-tap`, ADR-0054 point 4).
pub const BREW_FORMULA: &str = "nuetzliches/tap/devstack";

/// Homebrew's prefix (`/opt/homebrew`, `/usr/local`,
/// `/home/linuxbrew/.linuxbrew`) when Homebrew installed `exe`: the binary
/// lives in `<prefix>/Cellar/devstack/<version>/bin/`, and `<prefix>/bin/`
/// links to it. `exe` may be either.
pub fn homebrew_prefix(exe: &Path) -> Option<PathBuf> {
    cellar_prefix(exe).or_else(|| cellar_prefix(&std::fs::canonicalize(exe).ok()?))
}

/// `<prefix>` of `<prefix>/Cellar/devstack/<version>/bin/<file>`.
fn cellar_prefix(path: &Path) -> Option<PathBuf> {
    let parts: Vec<_> = path.components().collect();
    let at = parts.iter().rposition(|c| c.as_os_str() == "Cellar")?;
    let tail = &parts[at + 1..];
    (at > 0 && tail.len() == 4 && tail[0].as_os_str() == "devstack" && tail[2].as_os_str() == "bin")
        .then(|| parts[..at].iter().collect())
}

/// The path Homebrew keeps for devstack across upgrades,
/// `<prefix>/bin/devstack` (`<prefix>/opt/devstack/bin/devstack` for a keg
/// that is not linked), never the versioned one under `Cellar/`, which
/// `brew cleanup` removes.
pub fn brew_bin(prefix: &Path) -> PathBuf {
    let linked = prefix.join("bin").join("devstack");
    let opt = prefix
        .join("opt")
        .join("devstack")
        .join("bin")
        .join("devstack");
    if !linked.exists() && opt.exists() {
        opt
    } else {
        linked
    }
}

/// `brew` itself, of the prefix that installed devstack: a hub started at
/// logon may not have it on `PATH`.
pub fn brew(prefix: &Path) -> PathBuf {
    let own = prefix.join("bin").join("brew");
    if own.is_file() {
        own
    } else {
        PathBuf::from("brew")
    }
}

/// What starts this devstack again later (the hub at logon, the hub after an
/// update): `exe`, or for a copy Homebrew installed the path Homebrew keeps.
pub fn stable_exe(exe: &Path) -> PathBuf {
    match homebrew_prefix(exe) {
        Some(prefix) => brew_bin(&prefix),
        None => exe.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_homebrew_install_is_recognised() {
        let p = |s: &str| cellar_prefix(Path::new(s));
        assert_eq!(
            p("/opt/homebrew/Cellar/devstack/0.19.0/bin/devstack"),
            Some(PathBuf::from("/opt/homebrew"))
        );
        assert_eq!(
            p("/usr/local/Cellar/devstack/0.20.1/bin/devstack"),
            Some(PathBuf::from("/usr/local"))
        );
        assert_eq!(
            p("/home/linuxbrew/.linuxbrew/Cellar/devstack/0.19.0_1/bin/devstack"),
            Some(PathBuf::from("/home/linuxbrew/.linuxbrew"))
        );
        for not in [
            "/opt/homebrew/Cellar/other/1.0/bin/devstack",
            "/opt/homebrew/Cellar/devstack/0.19.0/libexec/devstack",
            "/Users/me/.local/bin/devstack",
            "/Users/me/src/Cellar/devstack/target/debug/devstack",
        ] {
            assert_eq!(p(not), None, "{not}");
        }
    }

    #[test]
    fn a_homebrew_install_is_started_by_the_path_homebrew_keeps() {
        assert_eq!(
            stable_exe(Path::new(
                "/opt/homebrew/Cellar/devstack/0.19.0/bin/devstack"
            )),
            PathBuf::from("/opt/homebrew/bin/devstack")
        );
        let other = Path::new("/Users/me/.local/bin/devstack");
        assert_eq!(stable_exe(other), other);
        // A keg that is not linked: its `opt` link.
        let d = tempfile::tempdir().unwrap();
        let opt = d.path().join("opt/devstack/bin");
        std::fs::create_dir_all(&opt).unwrap();
        std::fs::write(opt.join("devstack"), b"x").unwrap();
        assert_eq!(brew_bin(d.path()), opt.join("devstack"));
        std::fs::create_dir_all(d.path().join("bin")).unwrap();
        std::fs::write(d.path().join("bin/devstack"), b"x").unwrap();
        assert_eq!(brew_bin(d.path()), d.path().join("bin").join("devstack"));
    }

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
