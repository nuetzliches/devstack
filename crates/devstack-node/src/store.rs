//! The coordinator's members (ADR-0031 point 2), in `members.json` of its
//! directory. The running coordinator and the administrator's
//! `devstack node members approve` both change the file, two processes: every
//! change holds a lock on `members.lock` (a file lock, Windows too), reads
//! the file, and writes it whole (a temporary file of its own, then rename).
//! Reading is cached for as long as the file is unchanged.
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
    /// Set by admins (ADR-0033): who a member is for nodes' policies
    /// (`team:backend`), what a node offers (`gpu`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// As a node: what it lets run on it (ADR-0033).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<crate::policy::NodePolicy>,
    /// An admin granted it to run the pool's lanes: only then does the
    /// coordinator take its node API and place lanes on it (ADR-0032,
    /// addendum 2026-10-17).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub node: bool,
    /// An admin allowed it to cause provisioning (ADR-0051 point 3): a lane
    /// no node can take makes a machine, which costs someone money.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provision: bool,
    /// It keeps its lanes off provisioned nodes (ADR-0051 point 10).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_provisioned: bool,
    /// A node a provisioner made, enrolled with a token (ADR-0051 point 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioned: Option<Provisioned>,
}

/// What the coordinator knows of a node a provisioner made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provisioned {
    /// The coordinator's request it came from.
    pub request: String,
    /// The provider's id of the machine.
    pub machine: String,
    /// The address it enrolled from.
    pub address: String,
    /// An admin confirmed it, where the pool asks for that
    /// (`provision_confirm = "admin"`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirmed: bool,
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
    /// `approve`, `reject`, `remove`, `role`, `pause`, `resume`, `end`,
    /// `tags`, `policy`, `provisioning`, `upstream`, `upstream-remove`.
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
    /// Serialises the changes made by this process (the file lock does
    /// between processes).
    lock: Mutex<()>,
    /// The members as last read, and the file's length and time then.
    cache: Mutex<Option<(u64, std::time::SystemTime, Vec<Member>)>>,
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
            cache: Mutex::new(None),
        }
    }

    pub fn list(&self) -> Result<Vec<Member>, StoreError> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::Io(self.path.clone(), e)),
        };
        let stamp = (meta.len(), meta.modified().ok());
        if let (len, Some(at)) = stamp
            && let Some((l, t, ms)) = self.cache.lock().unwrap().as_ref()
            && *l == len
            && *t == at
        {
            return Ok(ms.clone());
        }
        let ms: Vec<Member> = match std::fs::read(&self.path) {
            Ok(b) => {
                serde_json::from_slice(&b).map_err(|e| StoreError::Parse(self.path.clone(), e))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::Io(self.path.clone(), e)),
        };
        if let (len, Some(at)) = stamp {
            *self.cache.lock().unwrap() = Some((len, at, ms.clone()));
        }
        Ok(ms)
    }

    fn write(&self, members: &[Member]) -> Result<(), StoreError> {
        // A name of its own: two writers never share a temporary file.
        let tmp = self.path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            crate::coordinator::random_hex(4)
        ));
        let io = |e| StoreError::Io(self.path.clone(), e);
        let written = std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(members).expect("members serialise"),
        )
        .and_then(|_| std::fs::rename(&tmp, &self.path));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        *self.cache.lock().unwrap() = None;
        written.map_err(io)
    }

    /// Change the members under the lock (this process's and the file's,
    /// for the other process), and write them if `f` says so.
    fn change<T>(
        &self,
        f: impl FnOnce(&mut Vec<Member>) -> Result<(T, bool), StoreError>,
    ) -> Result<T, StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let lock_path = self.path.with_extension("lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| StoreError::Io(lock_path.clone(), e))?;
        lock.lock()
            .map_err(|e| StoreError::Io(lock_path.clone(), e))?;
        // Read under the lock, never from the cache: the other process may
        // have written within the file time's resolution.
        *self.cache.lock().unwrap() = None;
        let mut members = self.list()?;
        let (out, changed) = f(&mut members)?;
        if changed {
            self.write(&members)?;
        }
        drop(lock);
        Ok(out)
    }

    /// How many requests wait for an admin.
    pub fn pending(&self) -> Result<usize, StoreError> {
        Ok(self
            .list()?
            .iter()
            .filter(|m| m.state == State::Pending)
            .count())
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
                tags: Vec::new(),
                policy: None,
                node: false,
                provision: false,
                no_provisioned: false,
                provisioned: None,
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

    /// Grant an approved member to run the pool's lanes, or take it back
    /// (ADR-0032, addendum 2026-10-17).
    pub fn set_node(&self, key: &str, node: bool, by: &str) -> Result<Member, StoreError> {
        let (m, changed) = self.change(|ms| {
            let i = Self::find(ms, key)?;
            if node && ms[i].state != State::Approved {
                return Err(StoreError::Refused(format!(
                    "{} ({}) is {}: only an approved member runs lanes",
                    ms[i].id,
                    ms[i].name,
                    ms[i].state.as_str()
                )));
            }
            let changed = ms[i].node != node;
            ms[i].node = node;
            Ok(((ms[i].clone(), changed), changed))
        })?;
        if changed {
            let detail = if node {
                "may run lanes"
            } else {
                "runs no lanes"
            };
            self.log(by, "node", &m, Some(detail.into()))?;
        }
        Ok(m)
    }

    /// Allow an approved member to cause provisioning, or take it back
    /// (ADR-0051 point 3).
    pub fn set_provision(&self, key: &str, on: bool, by: &str) -> Result<Member, StoreError> {
        let (m, changed) = self.change(|ms| {
            let i = Self::find(ms, key)?;
            if on && ms[i].state != State::Approved {
                return Err(StoreError::Refused(format!(
                    "{} ({}) is {}: only an approved member starts lanes",
                    ms[i].id,
                    ms[i].name,
                    ms[i].state.as_str()
                )));
            }
            let changed = ms[i].provision != on;
            ms[i].provision = on;
            Ok(((ms[i].clone(), changed), changed))
        })?;
        if changed {
            let detail = if on {
                "may cause provisioning"
            } else {
                "causes no provisioning"
            };
            self.log(by, "provision", &m, Some(detail.into()))?;
        }
        Ok(m)
    }

    /// A member keeps its lanes off provisioned nodes, or lets them go there
    /// again (ADR-0051 point 10): its own choice.
    pub fn set_no_provisioned(&self, id: &str, off: bool) -> Result<Member, StoreError> {
        self.change(|ms| {
            let i = ms
                .iter()
                .position(|m| m.id == id)
                .ok_or_else(|| StoreError::Unknown(id.into()))?;
            let changed = ms[i].no_provisioned != off;
            ms[i].no_provisioned = off;
            Ok((ms[i].clone(), changed))
        })
    }

    /// An admin confirms a provisioned node (ADR-0051 point 6).
    pub fn confirm(&self, key: &str, by: &str) -> Result<Member, StoreError> {
        let (m, changed) = self.change(|ms| {
            let i = Self::find(ms, key)?;
            let Some(p) = ms[i].provisioned.as_mut() else {
                return Err(StoreError::Refused(format!(
                    "{} ({}) is no provisioned node",
                    ms[i].id, ms[i].name
                )));
            };
            let changed = !p.confirmed;
            p.confirmed = true;
            Ok(((ms[i].clone(), changed), changed))
        })?;
        if changed {
            let p = m.provisioned.as_ref().expect("provisioned");
            self.log(
                by,
                "confirm",
                &m,
                Some(format!("machine {} at {}", p.machine, p.address)),
            )?;
        }
        Ok(m)
    }

    /// A machine a provisioner made joins with its enrolment (ADR-0051
    /// point 6): approved, granted as a node, marked provisioned, and (for a
    /// pool that provisions per repository) bound to the repository by its
    /// policy, in one step. A key that asked before is refused: a machine
    /// enrols once, with a key of its own.
    #[allow(clippy::too_many_arguments)]
    pub fn enrol(
        &self,
        id: &str,
        name: &str,
        csr: &str,
        address: &str,
        ca: &crate::pki::Pair,
        provisioned: Provisioned,
        repo: Option<&str>,
    ) -> Result<Member, StoreError> {
        let m = self.change(|ms| {
            if ms.iter().any(|m| m.id == id) {
                return Err(StoreError::Refused(format!(
                    "{id} asked to join before: a machine enrols with a key of its own"
                )));
            }
            let cert = crate::pki::sign_member(ca, csr, id)?;
            let m = Member {
                id: id.into(),
                name: name.into(),
                state: State::Approved,
                role: Role::Member,
                requested_at: now(),
                address: Some(address.into()),
                csr: csr.into(),
                cert_fp: Some(crate::pki::fingerprint(&cert)?),
                cert: Some(cert),
                paused: false,
                tags: Vec::new(),
                policy: repo.map(|r| crate::policy::NodePolicy {
                    repos: vec![r.to_string()],
                    ..crate::policy::NodePolicy::default()
                }),
                node: true,
                provision: false,
                no_provisioned: false,
                provisioned: Some(provisioned),
            };
            ms.push(m.clone());
            Ok((m, true))
        })?;
        let p = m.provisioned.as_ref().expect("provisioned");
        self.log(
            &format!("enrolment of request {}", p.request),
            "enrol",
            &m,
            Some(format!("machine {} at {}", p.machine, address)),
        )?;
        Ok(m)
    }

    /// Set a member's tags (ADR-0033): trimmed, sorted, without repeats.
    pub fn set_tags(&self, key: &str, tags: &[String], by: &str) -> Result<Member, StoreError> {
        let mut tags: Vec<String> = tags
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        tags.sort();
        tags.dedup();
        let (m, changed) = self.change(|ms| {
            let i = Self::find(ms, key)?;
            let changed = ms[i].tags != tags;
            ms[i].tags = tags.clone();
            Ok(((ms[i].clone(), changed), changed))
        })?;
        if changed {
            self.log(by, "tags", &m, Some(m.tags.join(", ")))?;
        }
        Ok(m)
    }

    /// Set what a node lets run on it (ADR-0033); `None` lifts it.
    pub fn set_policy(
        &self,
        key: &str,
        policy: Option<crate::policy::NodePolicy>,
        by: &str,
    ) -> Result<Member, StoreError> {
        let (m, changed) = self.change(|ms| {
            let i = Self::find(ms, key)?;
            let changed = ms[i].policy != policy;
            ms[i].policy = policy.clone();
            Ok(((ms[i].clone(), changed), changed))
        })?;
        if changed {
            let detail = serde_json::to_string(&m.policy).unwrap_or_default();
            self.log(by, "policy", &m, Some(detail))?;
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

    #[test]
    fn an_admin_grants_a_node() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path());
        let ca = pki::new_ca().unwrap();
        let a = ask(&s, "a");
        assert!(matches!(
            s.set_node(&a, true, SHELL),
            Err(StoreError::Refused(_))
        ));
        s.approve(&a, &ca, None, SHELL).unwrap();
        assert!(s.set_node(&a, true, SHELL).unwrap().node);
        assert!(s.get(&a).unwrap().unwrap().node);
        assert!(!s.set_node(&a, false, SHELL).unwrap().node);
        assert_eq!(s.audit(1).unwrap()[0].action, "node");
    }

    #[test]
    fn two_stores_on_one_file_lose_no_change() {
        // The coordinator and the CLI: two writers at once, each with its
        // own store (and process-local lock).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let s = Store::new(&path);
                    for i in 0..10 {
                        let id = format!("{t:02}{i:02}");
                        s.request(&id, &id, "csr", None).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(Store::new(&path).list().unwrap().len(), 40);
        let leftovers = std::fs::read_dir(&path)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
