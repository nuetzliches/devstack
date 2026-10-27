//! Where the hub keeps the secrets of the user's upstream MCP servers
//! (ADR-0069): their OAuth tokens and static bearer tokens. In the OS's
//! keychain (Windows Credential Manager, macOS Keychain, the Secret Service
//! on Linux), one entry per upstream. A Linux machine without a Secret
//! Service (a server, a headless session) has none: there they go into a
//! file only the user may read, beside the hub's other state, and
//! [`Secrets::kind`] says so for the dashboard to show.
//!
//! Nothing here leaves the hub: no route hands a secret out (no token
//! passthrough, ADR-0069).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// The keychain's service name; each upstream is an account under it.
const SERVICE: &str = "devstack";

/// Where [`Secrets`] keeps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The OS's keychain.
    Keychain,
    /// A file only the user may read (Linux without a Secret Service).
    File,
}

pub struct Secrets {
    store: Store,
}

enum Store {
    Keychain {
        service: String,
    },
    File {
        path: PathBuf,
        lock: Mutex<()>,
    },
    /// None: `devstack mcp` without a hub reads no secret (ADR-0069 point 6).
    Nothing,
}

impl Secrets {
    /// The keychain when it takes, gives back and forgets a probe entry;
    /// else (Linux only) the file `secrets.json` in `dir`.
    pub fn open(dir: PathBuf) -> Self {
        Self::open_as(SERVICE, dir)
    }

    fn open_as(service: &str, dir: PathBuf) -> Self {
        if probe(service) {
            return Self {
                store: Store::Keychain {
                    service: service.to_string(),
                },
            };
        }
        Self {
            store: Store::File {
                path: dir.join("secrets.json"),
                lock: Mutex::new(()),
            },
        }
    }

    /// A store that holds nothing and takes nothing.
    pub fn nothing() -> Self {
        Self {
            store: Store::Nothing,
        }
    }

    pub fn kind(&self) -> Kind {
        match self.store {
            Store::Keychain { .. } | Store::Nothing => Kind::Keychain,
            Store::File { .. } => Kind::File,
        }
    }

    /// The secret of `upstream`; `None` when there is none.
    pub fn get(&self, upstream: &str) -> Result<Option<String>, String> {
        match &self.store {
            Store::Keychain { service } => match entry(service, upstream)?.get_password() {
                Ok(s) => Ok(Some(s)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(e.to_string()),
            },
            Store::File { path, lock } => {
                let _held = lock.lock().unwrap();
                Ok(read_file(path)?.remove(upstream))
            }
            Store::Nothing => Ok(None),
        }
    }

    pub fn set(&self, upstream: &str, secret: &str) -> Result<(), String> {
        match &self.store {
            Store::Keychain { service } => entry(service, upstream)?
                .set_password(secret)
                .map_err(|e| e.to_string()),
            Store::File { path, lock } => {
                let _held = lock.lock().unwrap();
                let mut all = read_file(path)?;
                all.insert(upstream.to_string(), secret.to_string());
                write_file(path, &all)
            }
            Store::Nothing => Err("no secret is kept here".into()),
        }
    }

    /// Forget the secret of `upstream`; none there is no error.
    pub fn delete(&self, upstream: &str) -> Result<(), String> {
        match &self.store {
            Store::Keychain { service } => match entry(service, upstream)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.to_string()),
            },
            Store::File { path, lock } => {
                let _held = lock.lock().unwrap();
                let mut all = read_file(path)?;
                if all.remove(upstream).is_some() {
                    write_file(path, &all)?;
                }
                Ok(())
            }
            Store::Nothing => Ok(()),
        }
    }
}

fn entry(service: &str, upstream: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, &format!("upstream:{upstream}")).map_err(|e| e.to_string())
}

/// The keychain works here: a probe entry is written, read and removed.
/// Elsewhere than Linux it is the keychain even when the probe fails: a
/// locked keychain is said when a secret is used, not worked around.
fn probe(service: &str) -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    let Ok(e) = keyring::Entry::new(service, "probe") else {
        return false;
    };
    let ok = e.set_password("probe").is_ok() && e.get_password().is_ok_and(|p| p == "probe");
    let _ = e.delete_credential();
    ok
}

fn read_file(path: &std::path::Path) -> Result<BTreeMap<String, String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn write_file(path: &std::path::Path, all: &BTreeMap<String, String>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    crate::write_private(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against the real store of the machine the test runs on (the keychain
    /// on Windows and macOS, either on Linux), under a service of its own.
    #[test]
    fn a_secret_is_kept_given_back_and_forgotten() {
        let d = tempfile::tempdir().unwrap();
        let service = format!("devstack-test-{}", std::process::id());
        let s = Secrets::open_as(&service, d.path().to_path_buf());
        if !cfg!(target_os = "linux") {
            assert_eq!(s.kind(), Kind::Keychain);
        }
        assert_eq!(s.get("tracker").unwrap(), None);
        s.set("tracker", "token-1").unwrap();
        s.set("tracker", "token-2").unwrap();
        s.set("wiki", "token-w").unwrap();
        assert_eq!(s.get("tracker").unwrap().as_deref(), Some("token-2"));
        assert_eq!(s.get("wiki").unwrap().as_deref(), Some("token-w"));
        s.delete("tracker").unwrap();
        s.delete("tracker").unwrap();
        assert_eq!(s.get("tracker").unwrap(), None);
        assert_eq!(s.get("wiki").unwrap().as_deref(), Some("token-w"));
        s.delete("wiki").unwrap();
        assert_eq!(s.get("wiki").unwrap(), None);
    }

    #[test]
    fn the_file_is_the_users_alone() {
        let d = tempfile::tempdir().unwrap();
        let s = Secrets {
            store: Store::File {
                path: d.path().join("hub/secrets.json"),
                lock: Mutex::new(()),
            },
        };
        s.set("tracker", "token").unwrap();
        assert_eq!(s.get("tracker").unwrap().as_deref(), Some("token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.path().join("hub/secrets.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        s.delete("tracker").unwrap();
        assert_eq!(s.get("tracker").unwrap(), None);
    }
}
