//! The coordinator's members (ADR-0031 point 2), in `members.json` of its
//! directory. The running coordinator and the administrator's
//! `devstack node members approve` both change the file, so every change reads
//! it first and writes it whole (write to a temporary file, then rename).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Asked to join, not approved yet.
    Pending,
    Approved,
    /// Removed by the administrator: its certificate is refused from now on.
    Removed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Removed => "removed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    /// A short hash of the member's public key (`pki::member_id`).
    pub id: String,
    /// The machine's name, as it introduced itself.
    pub name: String,
    pub state: State,
    /// Unix seconds.
    pub requested_at: u64,
    pub csr: String,
    /// The client certificate, once approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    /// Its SHA-256, what a connection is recognised by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_fp: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Parse(PathBuf, serde_json::Error),
    #[error("no member `{0}`")]
    Unknown(String),
    #[error("`{0}` is ambiguous: {1}")]
    Ambiguous(String, String),
    #[error("{0}")]
    Pki(#[from] crate::pki::PkiError),
}

pub struct Store {
    path: PathBuf,
    /// Serialises the changes made by this process.
    lock: Mutex<()>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Store {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("members.json"),
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<Member>, StoreError> {
        match std::fs::read(&self.path) {
            Ok(b) => {
                serde_json::from_slice(&b).map_err(|e| StoreError::Parse(self.path.clone(), e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(StoreError::Io(self.path.clone(), e)),
        }
    }

    fn write(&self, members: &[Member]) -> Result<(), StoreError> {
        let tmp = self.path.with_extension("json.tmp");
        let io = |e| StoreError::Io(self.path.clone(), e);
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(members).expect("members serialise"),
        )
        .map_err(io)?;
        std::fs::rename(&tmp, &self.path).map_err(io)
    }

    /// Change the members under the lock, and write them if `f` says so.
    fn change<T>(
        &self,
        f: impl FnOnce(&mut Vec<Member>) -> Result<(T, bool), StoreError>,
    ) -> Result<T, StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut members = self.list()?;
        let (out, changed) = f(&mut members)?;
        if changed {
            self.write(&members)?;
        }
        Ok(out)
    }

    /// A request to join. The same key asks as the same member: a pending
    /// request is renewed (a new name), an approved or removed one stays as
    /// it is.
    pub fn request(&self, id: &str, name: &str, csr: &str) -> Result<Member, StoreError> {
        self.change(|ms| {
            if let Some(m) = ms.iter_mut().find(|m| m.id == id) {
                if m.state == State::Pending && (m.name != name || m.csr != csr) {
                    m.name = name.into();
                    m.csr = csr.into();
                    return Ok((m.clone(), true));
                }
                return Ok((m.clone(), false));
            }
            let m = Member {
                id: id.into(),
                name: name.into(),
                state: State::Pending,
                requested_at: now(),
                csr: csr.into(),
                cert: None,
                cert_fp: None,
            };
            ms.push(m.clone());
            Ok((m, true))
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Member>, StoreError> {
        Ok(self.list()?.into_iter().find(|m| m.id == id))
    }

    /// The approved member a client certificate belongs to.
    pub fn by_cert(&self, fp: &str) -> Result<Option<Member>, StoreError> {
        Ok(self
            .list()?
            .into_iter()
            .find(|m| m.state == State::Approved && m.cert_fp.as_deref() == Some(fp)))
    }

    /// A member by its id or a unique prefix of it, or by its name when that
    /// is unique.
    fn find<'a>(ms: &'a mut [Member], key: &str) -> Result<&'a mut Member, StoreError> {
        let pick = |f: &dyn Fn(&Member) -> bool| -> Vec<usize> {
            (0..ms.len()).filter(|&i| f(&ms[i])).collect()
        };
        let mut hits = pick(&|m| m.id == key);
        if hits.is_empty() {
            hits = pick(&|m| m.id.starts_with(key));
        }
        if hits.is_empty() {
            hits = pick(&|m| m.name == key);
        }
        match hits.as_slice() {
            [] => Err(StoreError::Unknown(key.into())),
            [i] => Ok(&mut ms[*i]),
            many => Err(StoreError::Ambiguous(
                key.into(),
                many.iter()
                    .map(|&i| format!("{} ({})", ms[i].id, ms[i].name))
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
        }
    }

    /// Approve a member: sign its request with the pool's CA.
    pub fn approve(&self, key: &str, ca: &crate::pki::Pair) -> Result<Member, StoreError> {
        self.change(|ms| {
            let m = Self::find(ms, key)?;
            if m.state == State::Approved {
                return Ok((m.clone(), false));
            }
            let cert = crate::pki::sign_member(ca, &m.csr, &m.id)?;
            m.cert_fp = Some(crate::pki::fingerprint(&cert)?);
            m.cert = Some(cert);
            m.state = State::Approved;
            Ok((m.clone(), true))
        })
    }

    /// Remove a member: its certificate is refused from now on. Joining
    /// again needs a new key.
    pub fn remove(&self, key: &str) -> Result<Member, StoreError> {
        self.change(|ms| {
            let m = Self::find(ms, key)?;
            let changed = m.state != State::Removed;
            m.state = State::Removed;
            Ok((m.clone(), changed))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki;

    #[test]
    fn a_member_asks_is_approved_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path());
        let ca = pki::new_ca().unwrap();
        let (_k, csr) = pki::member_request("pc").unwrap();
        let id = pki::member_id(&csr).unwrap();
        assert_eq!(s.request(&id, "pc", &csr).unwrap().state, State::Pending);
        assert_eq!(s.request(&id, "pc", &csr).unwrap().state, State::Pending);
        assert_eq!(s.list().unwrap().len(), 1, "asking twice is one member");

        let m = s.approve(&id[..6], &ca).unwrap();
        assert_eq!(m.state, State::Approved);
        let fp = m.cert_fp.unwrap();
        assert_eq!(s.by_cert(&fp).unwrap().unwrap().id, id);
        assert_eq!(s.request(&id, "renamed", &csr).unwrap().name, "pc");

        s.remove("pc").unwrap();
        assert!(
            s.by_cert(&fp).unwrap().is_none(),
            "a removed member is refused"
        );
        assert!(matches!(
            s.approve("nobody", &ca),
            Err(StoreError::Unknown(_))
        ));
    }
}
