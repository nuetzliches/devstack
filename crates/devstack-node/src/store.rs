//! The coordinator's members (ADR-0031 point 2), in `members.json` of its
//! directory. The running coordinator and the administrator's
//! `devstack node members approve` both change the file, so every change reads
//! it first and writes it whole (write to a temporary file, then rename).
//! Every change an administrator makes is appended to `audit.jsonl`
//! (ADR-0032 point 5).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Asked to join, not approved yet.
    Pending,
    Approved,
    /// Removed (or its request rejected): its certificate is refused from
    /// now on.
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

/// What a member may do in the pool (ADR-0032).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    Member,
    /// Approves, rejects and removes members, changes roles, manages nodes
    /// and leases.
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "member" => Ok(Self::Member),
            "admin" => Ok(Self::Admin),
            _ => Err(format!("`{s}` is no role: member or admin")),
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
    #[serde(default)]
    pub role: Role,
    /// Unix seconds.
    pub requested_at: u64,
    /// The address the request came from, for the administrator's eyes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub csr: String,
    /// The client certificate, once approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    /// Its SHA-256, what a connection is recognised by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_fp: Option<String>,
    /// As a node: paused by an admin, no new lanes (running ones stay).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
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
    /// A change the pool's rules do not allow.
    #[error("{0}")]
    Refused(String),
}

/// One administrator's change, a line of `audit.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Unix seconds.
    pub at: u64,
    /// Who: a member (`id (name)`) or [`SHELL`].
    pub by: String,
    /// `approve`, `reject`, `remove`, `role`, `pause`, `resume`, `end`.
    pub action: String,
    pub member: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Who acts when the change comes from the coordinator's own shell.
pub const SHELL: &str = "shell on the coordinator";

pub struct Store {
    path: PathBuf,
    audit: PathBuf,
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
            audit: dir.join("audit.jsonl"),
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
    /// request is renewed (a new name or address), an approved or removed one
    /// stays as it is.
    pub fn request(
        &self,
        id: &str,
        name: &str,
        csr: &str,
        address: Option<&str>,
    ) -> Result<Member, StoreError> {
        let address = address.map(str::to_string);
        self.change(|ms| {
            if let Some(m) = ms.iter_mut().find(|m| m.id == id) {
                if m.state == State::Pending
                    && (m.name != name || m.csr != csr || m.address != address)
                {
                    m.name = name.into();
                    m.csr = csr.into();
                    m.address = address;
                    return Ok((m.clone(), true));
                }
                return Ok((m.clone(), false));
            }
            let m = Member {
                id: id.into(),
                name: name.into(),
                state: State::Pending,
                role: Role::Member,
                requested_at: now(),
                address,
                csr: csr.into(),
                cert: None,
                cert_fp: None,
                paused: false,
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

    /// The index of a member by its id or a unique prefix of it, or by its
    /// name when that is unique.
    fn find(ms: &[Member], key: &str) -> Result<usize, StoreError> {
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
            [i] => Ok(*i),
            many => Err(StoreError::Ambiguous(
                key.into(),
                many.iter()
                    .map(|&i| format!("{} ({})", ms[i].id, ms[i].name))
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
        }
    }

    /// Whether member `i` is the pool's only approved admin (who must stay
    /// one).
    fn last_admin(ms: &[Member], i: usize) -> bool {
        let admin = |m: &Member| m.state == State::Approved && m.role == Role::Admin;
        admin(&ms[i]) && ms.iter().filter(|m| admin(m)).count() == 1
    }

    fn log(
        &self,
        by: &str,
        action: &str,
        m: &Member,
        detail: Option<String>,
    ) -> Result<(), StoreError> {
        use std::io::Write;
        let e = AuditEntry {
            at: now(),
            by: by.into(),
            action: action.into(),
            member: m.id.clone(),
            name: m.name.clone(),
            detail,
        };
        let mut line = serde_json::to_string(&e).expect("an audit entry serialises");
        line.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .map_err(|e| StoreError::Io(self.audit.clone(), e))
    }

    /// The last `limit` administrator changes, newest first.
    pub fn audit(&self, limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
        let text = match std::fs::read_to_string(&self.audit) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::Io(self.audit.clone(), e)),
        };
        Ok(text
            .lines()
            .rev()
            .filter_map(|l| serde_json::from_str(l).ok())
            .take(limit)
            .collect())
    }

    /// Approve a member: sign its request with the pool's CA, as `role`
    /// (default: a member). Approving an approved member again only applies
    /// a role given here.
    pub fn approve(
        &self,
        key: &str,
        ca: &crate::pki::Pair,
        role: Option<Role>,
        by: &str,
    ) -> Result<Member, StoreError> {
        let approved = self.change(|ms| {
            let i = Self::find(ms, key)?;
            let m = &mut ms[i];
            match m.state {
                State::Approved => return Ok((None, false)),
                State::Removed => {
                    return Err(StoreError::Refused(format!(
                        "{} ({}) was removed; it joins again with a new key",
                        m.id, m.name
                    )));
                }
                State::Pending => {}
            }
            let cert = crate::pki::sign_member(ca, &m.csr, &m.id)?;
            m.cert_fp = Some(crate::pki::fingerprint(&cert)?);
            m.cert = Some(cert);
            m.state = State::Approved;
            m.role = role.unwrap_or_default();
            Ok((Some(m.clone()), true))
        })?;
        match (approved, role) {
            (Some(m), _) => {
                self.log(by, "approve", &m, Some(m.role.as_str().into()))?;
                Ok(m)
            }
            (None, Some(r)) => self.set_role(key, r, by),
            (None, None) => {
                let ms = self.list()?;
                Ok(ms[Self::find(&ms, key)?].clone())
            }
        }
    }

    /// Change an approved member's role. The last admin stays one.
    pub fn set_role(&self, key: &str, role: Role, by: &str) -> Result<Member, StoreError> {
        let (m, changed) = self.change(|ms| {
            let i = Self::find(ms, key)?;
            if ms[i].state != State::Approved {
                return Err(StoreError::Refused(format!(
                    "{} ({}) is {}: only an approved member has a role",
                    ms[i].id,
                    ms[i].name,
                    ms[i].state.as_str()
                )));
            }
            if role != Role::Admin && Self::last_admin(ms, i) {
                return Err(StoreError::Refused(
                    "the pool's last admin stays one: make another member admin first".into(),
                ));
            }
            let changed = ms[i].role != role;
            ms[i].role = role;
            Ok(((ms[i].clone(), changed), changed))
        })?;
        if changed {
            self.log(by, "role", &m, Some(role.as_str().into()))?;
        }
        Ok(m)
    }

    /// Pause a node, or let it take lanes again (ADR-0032 point 4).
    pub fn set_paused(&self, key: &str, paused: bool, by: &str) -> Result<Member, StoreError> {
        let (m, changed) = self.change(|ms| {
            let i = Self::find(ms, key)?;
            if ms[i].state != State::Approved {
                return Err(StoreError::Refused(format!(
                    "{} ({}) is {}: only an approved member runs lanes",
                    ms[i].id,
                    ms[i].name,
                    ms[i].state.as_str()
                )));
            }
            let changed = ms[i].paused != paused;
            ms[i].paused = paused;
            Ok(((ms[i].clone(), changed), changed))
        })?;
        if changed {
            self.log(by, if paused { "pause" } else { "resume" }, &m, None)?;
        }
        Ok(m)
    }

    /// Record an admin's change that is not one of the members' (ending a
    /// lease), against the member it concerns.
    pub fn record(
        &self,
        by: &str,
        action: &str,
        member: &str,
        detail: Option<String>,
    ) -> Result<(), StoreError> {
        let ms = self.list()?;
        let m = &ms[Self::find(&ms, member)?];
        self.log(by, action, m, detail)
    }

    /// Remove a member, or reject its request: its certificate is refused
    /// from now on, and joining again needs a new key. The last admin cannot
    /// be removed.
    pub fn remove(&self, key: &str, by: &str) -> Result<Member, StoreError> {
        let (m, action) = self.change(|ms| {
            let i = Self::find(ms, key)?;
            if Self::last_admin(ms, i) {
                return Err(StoreError::Refused(
                    "the pool's last admin cannot be removed: make another member admin first"
                        .into(),
                ));
            }
            let action = match ms[i].state {
                State::Removed => return Ok(((ms[i].clone(), None), false)),
                State::Pending => "reject",
                State::Approved => "remove",
            };
            ms[i].state = State::Removed;
            Ok(((ms[i].clone(), Some(action)), true))
        })?;
        if let Some(a) = action {
            self.log(by, a, &m, None)?;
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki;

    fn ask(s: &Store, name: &str) -> String {
        let (_k, csr) = pki::member_request(name).unwrap();
        let id = pki::member_id(&csr).unwrap();
        s.request(&id, name, &csr, Some("10.0.0.7")).unwrap();
        id
    }

    #[test]
    fn a_member_asks_is_approved_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path());
        let ca = pki::new_ca().unwrap();
        let (_k, csr) = pki::member_request("pc").unwrap();
        let id = pki::member_id(&csr).unwrap();
        assert_eq!(
            s.request(&id, "pc", &csr, None).unwrap().state,
            State::Pending
        );
        assert_eq!(
            s.request(&id, "pc", &csr, None).unwrap().state,
            State::Pending
        );
        assert_eq!(s.list().unwrap().len(), 1, "asking twice is one member");

        let m = s.approve(&id[..6], &ca, None, SHELL).unwrap();
        assert_eq!(m.state, State::Approved);
        assert_eq!(m.role, Role::Member);
        let fp = m.cert_fp.unwrap();
        assert_eq!(s.by_cert(&fp).unwrap().unwrap().id, id);
        assert_eq!(s.request(&id, "renamed", &csr, None).unwrap().name, "pc");

        s.remove("pc", SHELL).unwrap();
        assert!(
            s.by_cert(&fp).unwrap().is_none(),
            "a removed member is refused"
        );
        assert!(matches!(
            s.approve("nobody", &ca, None, SHELL),
            Err(StoreError::Unknown(_))
        ));
    }

    #[test]
    fn the_last_admin_stays_and_every_change_is_logged() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path());
        let ca = pki::new_ca().unwrap();
        let (a, b, c) = (ask(&s, "a"), ask(&s, "b"), ask(&s, "c"));
        assert_eq!(
            s.get(&a).unwrap().unwrap().address.as_deref(),
            Some("10.0.0.7")
        );

        s.approve(&a, &ca, Some(Role::Admin), SHELL).unwrap();
        assert!(matches!(
            s.set_role(&a, Role::Member, "x"),
            Err(StoreError::Refused(_))
        ));
        assert!(matches!(s.remove(&a, "x"), Err(StoreError::Refused(_))));

        s.approve(&b, &ca, None, "a").unwrap();
        s.approve(&b, &ca, Some(Role::Admin), "a").unwrap();
        assert_eq!(s.get(&b).unwrap().unwrap().role, Role::Admin);
        s.set_role(&a, Role::Member, "b").unwrap();
        s.remove(&c, "b").unwrap();
        assert!(matches!(
            s.approve(&c, &ca, None, "b"),
            Err(StoreError::Refused(_))
        ));

        let log: Vec<String> = s.audit(10).unwrap().into_iter().map(|e| e.action).collect();
        assert_eq!(log, ["reject", "role", "role", "approve", "approve"]);
    }
}
