//! The API token for CLI and MCP (ADR-0019): random, in the user's profile,
//! readable only by that user.

use std::io::Write;
use std::path::PathBuf;

/// The user's own state directory, if the environment names one.
fn profile_base() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData").join("Local"))
        });
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        });
    base.filter(|b| !b.as_os_str().is_empty())
}

pub fn token_path() -> PathBuf {
    profile_base()
        .unwrap_or_else(std::env::temp_dir)
        .join("devstack")
        .join("token")
}

/// Read the token, creating it on first use: created readable by this user
/// only from the start (never world-readable for a moment), and never in a
/// shared temporary directory, where another user could have put one.
pub fn load_or_create() -> std::io::Result<String> {
    if profile_base().is_none() {
        return Err(std::io::Error::other(
            "no profile directory (LOCALAPPDATA, USERPROFILE, XDG_STATE_HOME or HOME): \
             the API token would go to a directory other users can write",
        ));
    }
    let path = token_path();
    if let Some(t) = read_valid(&path)? {
        return Ok(t);
    }
    std::fs::create_dir_all(path.parent().expect("token path has a parent"))?;
    let bytes: [u8; 32] = rand::random();
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(&path) {
        Ok(mut f) => {
            f.write_all(token.as_bytes())?;
            Ok(token)
        }
        // Another process made it meanwhile (two daemons starting), or an
        // unusable one is there: theirs counts once it is written.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            for _ in 0..50 {
                if let Some(t) = read_valid(&path)? {
                    return Ok(t);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // Too short to be one of ours: replace it.
            let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
            let _ = std::fs::remove_file(&tmp);
            let mut f = opts.open(&tmp)?;
            f.write_all(token.as_bytes())?;
            drop(f);
            std::fs::rename(&tmp, &path)?;
            Ok(token)
        }
        Err(e) => Err(e),
    }
}

/// The token in `path`, if it holds one; on Unix readable by nobody else
/// (it is made so where it is not).
fn read_valid(path: &std::path::Path) -> std::io::Result<Option<String>> {
    let Ok(t) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let t = t.trim().to_string();
    if t.len() < 32 {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::metadata(path)?;
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(Some(t))
}

pub fn load() -> Option<String> {
    std::fs::read_to_string(token_path())
        .ok()
        .map(|t| t.trim().to_string())
}
