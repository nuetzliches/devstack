//! The skill sets this machine's pools hand out, installed into the coding
//! agents' user skill folders (ADR-0074); a skill bound to repositories
//! (`metadata.repos`, ADR-0079) into the agents' skill folders of the
//! working trees of those repositories instead, each folder it writes
//! there ignored by a `.gitignore` of its own.
//!
//! - A set is used only once its user accepted it, from the source it names
//!   then; a set whose source changed is offered again. Its updates come at
//!   once, or wait for the user's word, as the user chose for the set.
//! - The agents are those of the agent table (`devstack_skills::agents`)
//!   found on this machine; the user switches each one on. Several agents
//!   may share a folder (`~/.agents/skills`): it is written once.
//! - The hub writes only folders its lock names, with the content it wrote:
//!   a skill of the user's own of the same name, or one the user changed,
//!   stays as it is and is shown as a conflict. A skill no accepted set
//!   names any more goes.
//! - What changed is read when the pool's stream says so (`poolfeed`); the
//!   dashboard hears the state on `/hub/events` (`skills`).
//!
//! State in the hub's directory: `skills.json` (the user's choices and the
//! lock), `pool-skills.json` (the sets as last read), `skill-store/` (the
//! archives, by their SHA-256).

use devstack_node::skills::SetManifest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::sync::watch;

/// The user's word on a set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub pool: String,
    pub set: String,
    /// The source accepted or declined (`Source::label`).
    pub source: String,
    pub accepted: bool,
    /// Updates at once; else they wait for the user.
    #[serde(default)]
    pub auto: bool,
    /// The version of each skill the user agreed to (`auto` off).
    #[serde(default)]
    pub approved: BTreeMap<String, String>,
    /// The skills the user switched off (ADR-0077); every other one is on,
    /// one the set gains later too.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub off: BTreeSet<String>,
}

/// A working tree the hub follows (ADR-0078), and the URL of its `origin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub root: PathBuf,
    pub origin: Option<String>,
}

/// A folder a skill goes to: an agent's user folder, or (a skill bound to
/// repositories) an agent's folder in a working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    dir: PathBuf,
    /// The working tree it is in.
    tree: Option<PathBuf>,
    /// The agents switched on that read it.
    agents: Vec<String>,
}

/// A skill the hub installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// The agents' folder it is in.
    pub dir: PathBuf,
    pub name: String,
    pub pool: String,
    pub set: String,
    pub sha256: String,
    /// The hash of the files as written.
    pub tree: String,
    /// When this hub wrote it (Unix ms); 0 from a hub before ADR-0077.
    #[serde(default)]
    pub updated: u128,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Settings {
    /// Agent id: installing into its folder or not.
    #[serde(default)]
    agents: BTreeMap<String, bool>,
    #[serde(default)]
    sets: Vec<Decision>,
    #[serde(default)]
    installed: Vec<Installed>,
}

/// A skill's state, as the dashboard shows it.
#[derive(Debug, Clone, Serialize)]
pub struct SkillView {
    pub name: String,
    pub description: String,
    pub sha256: String,
    /// In every folder it goes to, as the set has it.
    pub installed: bool,
    /// A version the user has not agreed to yet (updates wait).
    pub pending: bool,
    /// Switched on (ADR-0077): on unless the user switched it off.
    pub enabled: bool,
    /// `metadata.group` and `metadata.tags` of its frontmatter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// When this hub last wrote it into an agent's folder (Unix ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<u128>,
    /// The agents whose folder holds it as the set has it.
    pub agents: Vec<String>,
    /// The repositories it is bound to (ADR-0079); none: every one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    /// The working trees it is in as the set has it (a bound skill).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trees: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetView {
    pub pool: String,
    pub set: String,
    pub source: String,
    /// `offered`, `accepted`, `declined`, or `changed` (the source is
    /// another than the one accepted).
    pub state: &'static str,
    pub auto: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Why the pool's last fetch failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub skills: Vec<SkillView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    pub id: String,
    pub name: String,
    pub dir: String,
    /// Its folder in a working tree, for the skills bound to repositories
    /// (ADR-0079).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conflict {
    pub dir: String,
    pub name: String,
    pub reason: String,
}

/// Everything the dashboard shows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct View {
    /// The agents found on this machine, and the ones switched on.
    pub agents: Vec<AgentView>,
    pub sets: Vec<SetView>,
    pub conflicts: Vec<Conflict>,
    /// Why the last sync failed in part (a download, a folder).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// When the last sync ended (Unix ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synced: Option<u128>,
}

/// Where an agent's skill folder is: the table's, or (tests) another.
type Folder = Box<dyn Fn(&devstack_skills::agents::Agent) -> Option<PathBuf> + Send + Sync>;
/// Whether an agent is on this machine.
type Found = Box<dyn Fn(&devstack_skills::agents::Agent) -> bool + Send + Sync>;
/// How a skill's archive is fetched: pool, set, skill.
type Download = Box<dyn Fn(&str, &str, &str) -> Result<Vec<u8>, String> + Send + Sync>;
/// The working trees the hub follows.
type Trees = Box<dyn Fn() -> Vec<Tree> + Send + Sync>;
/// Whether git tracks a file at or below a path (relative) of a tree.
type Tracked = Box<dyn Fn(&Path, &Path) -> bool + Send + Sync>;

/// What a skill name should be.
struct Want {
    pool: String,
    set: String,
    sha: String,
    repos: Vec<String>,
}
type Wanted = BTreeMap<String, Want>;

pub struct PoolSkills {
    dir: PathBuf,
    /// One change and one sync at a time.
    lock: Mutex<()>,
    tx: watch::Sender<View>,
    last: Mutex<(Vec<Conflict>, Vec<String>, Option<u128>)>,
    /// Where an agent's folder is: the table's, or (tests) another.
    folder: Folder,
    /// Whether an agent is on this machine.
    found: Found,
    /// How a skill's archive is fetched: pool, set, skill.
    download: Download,
    /// The working trees the hub follows, and the ones the last sync saw.
    trees: Trees,
    seen: Mutex<Vec<Tree>>,
    tracked: Tracked,
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// The hash of the files below `dir`, by path and content.
pub fn tree(dir: &Path) -> Option<String> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let t = e.file_type()?;
            if t.is_dir() {
                walk(&e.path(), &path, out)?;
            } else {
                out.push((path, e.path()));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, "", &mut files).ok()?;
    files.sort();
    let mut h = Sha256::new();
    for (rel, p) in files {
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(std::fs::read(p).ok()?);
        h.update([0]);
    }
    Some(devstack_node::skills::hex(&h.finalize()))
}

impl PoolSkills {
    /// The hub's: the table's folders, archives from the pools, the
    /// working trees of the stacks registered here.
    pub fn open(dir: PathBuf) -> Self {
        let mut s = Self::with(
            dir,
            Box::new(|a| a.skills_dir()),
            Box::new(|a| a.installed()),
            Box::new(|pool, set, skill| {
                let dir = devstack_node::member::pool_dir(&crate::poolfeed::state(), pool);
                devstack_node::member::Local::new(&dir)
                    .skill_archive(set, skill)
                    .map_err(|e| e.to_string())
            }),
        );
        s.trees = Box::new(registered_trees);
        s.tracked = Box::new(crate::branches::tracked);
        s
    }

    fn with(dir: PathBuf, folder: Folder, found: Found, download: Download) -> Self {
        let s = Self {
            dir,
            lock: Mutex::new(()),
            tx: watch::channel(View::default()).0,
            last: Mutex::new((Vec::new(), Vec::new(), None)),
            folder,
            found,
            download,
            trees: Box::new(Vec::new),
            seen: Mutex::new(Vec::new()),
            tracked: Box::new(|_, _| false),
        };
        s.publish();
        s
    }

    pub fn subscribe(&self) -> watch::Receiver<View> {
        self.tx.subscribe()
    }

    fn settings(&self) -> Settings {
        std::fs::read(self.dir.join("skills.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, s: &Settings) -> Result<(), String> {
        write(
            &self.dir.join("skills.json"),
            &serde_json::to_vec_pretty(s).expect("serialises"),
        )
    }

    fn lists(&self) -> BTreeMap<String, Vec<SetManifest>> {
        std::fs::read(self.dir.join("pool-skills.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The sets `pool` gives this machine now; `None`: this machine left
    /// it. Then installs what changed.
    pub fn set_pool(&self, pool: &str, sets: Option<Vec<SetManifest>>) {
        {
            let _one = self.lock.lock().unwrap();
            let mut l = self.lists();
            let before = l.clone();
            match sets {
                Some(s) => {
                    l.insert(pool.to_string(), s);
                }
                None => {
                    l.remove(pool);
                }
            }
            if l == before {
                drop(_one);
                self.publish();
                return;
            }
            let _ = write(
                &self.dir.join("pool-skills.json"),
                &serde_json::to_vec_pretty(&l).expect("serialises"),
            );
        }
        self.sync();
    }

    /// The agents in the table that are on this machine, or switched on.
    fn agents(&self, s: &Settings) -> Vec<AgentView> {
        devstack_skills::agents::all()
            .iter()
            .filter(|a| (self.found)(a) || s.agents.get(&a.id) == Some(&true))
            .filter_map(|a| {
                Some(AgentView {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    dir: (self.folder)(a)?.to_string_lossy().into_owned(),
                    project: a.project.clone(),
                    enabled: s.agents.get(&a.id) == Some(&true),
                })
            })
            .collect()
    }

    /// Switch installing into the agent `id`'s folder on or off.
    pub fn set_agent(&self, id: &str, on: bool) -> Result<(), String> {
        let a = devstack_skills::agents::get(id).ok_or_else(|| format!("no agent `{id}`"))?;
        if on && (self.folder)(a).is_none() {
            return Err(format!("{}: no skill folder on this machine", a.name));
        }
        {
            let _one = self.lock.lock().unwrap();
            let mut s = self.settings();
            s.agents.insert(id.to_string(), on);
            self.save(&s)?;
        }
        self.sync();
        self.local();
        Ok(())
    }

    /// The generated devstack skills (`devstack skills install`) in every
    /// working tree of the stacks registered here (ADR-0079): into the
    /// folders there of the agents switched on, and refreshed where an
    /// earlier one is; each folder ignores itself. A skill git tracks is
    /// left to the upgrade prompt.
    pub fn local(&self) {
        let s = self.settings();
        let mut rels: Vec<&str> = Vec::new();
        for a in devstack_skills::agents::all() {
            if s.agents.get(&a.id) == Some(&true)
                && let Some(p) = a.project.as_deref()
                && !rels.contains(&p)
            {
                rels.push(p);
            }
        }
        for e in devstack_daemon::registry::list() {
            let rel = e
                .file
                .strip_prefix(&e.repo)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| PathBuf::from("devstack.toml"));
            let mut roots = vec![e.repo.clone()];
            roots.extend(
                devstack_daemon::lanes::worktree_branches(&e.repo)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(p, _)| p)
                    .filter(|p| !devstack_daemon::lanes::same_tree(p, &e.repo)),
            );
            for root in roots {
                // A worktree without a stack file of its own runs with the
                // main tree's (ADR-0055); its skills say so too.
                let file = root.join(&rel);
                let Ok(stack) = devstack_config::load(if file.is_file() { &file } else { &e.file })
                else {
                    continue;
                };
                let base = file.parent().unwrap_or(&root).to_path_buf();
                let dirs: Vec<PathBuf> = rels
                    .iter()
                    .map(|r| {
                        let mut d = base.clone();
                        d.extend(r.split('/').filter(|p| !p.is_empty()));
                        d
                    })
                    .collect();
                let skip = |folder: &Path| {
                    folder
                        .strip_prefix(&root)
                        .is_ok_and(|rel| (self.tracked)(&root, rel))
                };
                if let Err(err) = devstack_skills::install_into(&stack, &file, &dirs, &skip) {
                    devstack_daemon::diag::warn(
                        "local_skills",
                        serde_json::json!({ "tree": root, "error": format!("{err:#}") }),
                    );
                }
            }
        }
    }

    fn manifest(&self, pool: &str, set: &str) -> Result<SetManifest, String> {
        self.lists()
            .remove(pool)
            .and_then(|l| l.into_iter().find(|m| m.name == set))
            .ok_or_else(|| format!("the pool `{pool}` gives this machine no set `{set}`"))
    }

    /// Accept the set: its skills as they are now, its updates at once
    /// (`auto`) or on the user's word.
    pub fn accept(&self, pool: &str, set: &str, auto: bool) -> Result<(), String> {
        {
            let _one = self.lock.lock().unwrap();
            let m = self.manifest(pool, set)?;
            let mut s = self.settings();
            s.sets.retain(|d| !(d.pool == pool && d.set == set));
            s.sets.push(Decision {
                pool: pool.into(),
                set: set.into(),
                source: m.source.clone(),
                accepted: true,
                auto,
                approved: m
                    .skills
                    .iter()
                    .map(|k| (k.name.clone(), k.sha256.clone()))
                    .collect(),
                off: BTreeSet::new(),
            });
            self.save(&s)?;
        }
        self.sync();
        Ok(())
    }

    /// Decline the set (and remove what was installed of it).
    pub fn decline(&self, pool: &str, set: &str) -> Result<(), String> {
        {
            let _one = self.lock.lock().unwrap();
            let m = self.manifest(pool, set)?;
            let mut s = self.settings();
            s.sets.retain(|d| !(d.pool == pool && d.set == set));
            s.sets.push(Decision {
                pool: pool.into(),
                set: set.into(),
                source: m.source,
                accepted: false,
                auto: false,
                approved: BTreeMap::new(),
                off: BTreeSet::new(),
            });
            self.save(&s)?;
        }
        self.sync();
        Ok(())
    }

    /// Updates of an accepted set at once, or on the user's word.
    pub fn set_auto(&self, pool: &str, set: &str, auto: bool) -> Result<(), String> {
        {
            let _one = self.lock.lock().unwrap();
            let mut s = self.settings();
            let d = s
                .sets
                .iter_mut()
                .find(|d| d.pool == pool && d.set == set && d.accepted)
                .ok_or_else(|| format!("`{pool}:{set}` is not accepted"))?;
            d.auto = auto;
            self.save(&s)?;
        }
        self.sync();
        Ok(())
    }

    /// Agree to the set's skills as they are now (its waiting updates).
    pub fn update(&self, pool: &str, set: &str) -> Result<(), String> {
        {
            let _one = self.lock.lock().unwrap();
            let m = self.manifest(pool, set)?;
            let mut s = self.settings();
            let d = s
                .sets
                .iter_mut()
                .find(|d| d.pool == pool && d.set == set && d.accepted && d.source == m.source)
                .ok_or_else(|| format!("`{pool}:{set}` is not accepted from its source"))?;
            d.approved = m
                .skills
                .iter()
                .map(|k| (k.name.clone(), k.sha256.clone()))
                .collect();
            self.save(&s)?;
        }
        self.sync();
        Ok(())
    }

    /// Switch the skill `name` of an accepted set on or off (ADR-0077):
    /// off, it goes from the agents' folders (unless changed there).
    pub fn set_skill(&self, pool: &str, set: &str, name: &str, on: bool) -> Result<(), String> {
        self.switch(pool, set, on, |k| k.name == name)
    }

    /// Switch every skill of `group` in an accepted set on or off; `""`:
    /// the skills without a group.
    pub fn set_group(&self, pool: &str, set: &str, group: &str, on: bool) -> Result<(), String> {
        self.switch(pool, set, on, |k| k.group.as_deref().unwrap_or("") == group)
    }

    fn switch(
        &self,
        pool: &str,
        set: &str,
        on: bool,
        which: impl Fn(&devstack_node::skills::SkillEntry) -> bool,
    ) -> Result<(), String> {
        {
            let _one = self.lock.lock().unwrap();
            let m = self.manifest(pool, set)?;
            let names: Vec<String> = m
                .skills
                .iter()
                .filter(|k| which(k))
                .map(|k| k.name.clone())
                .collect();
            if names.is_empty() {
                return Err(format!("`{pool}:{set}` has no such skill"));
            }
            let mut s = self.settings();
            let d = s
                .sets
                .iter_mut()
                .find(|d| d.pool == pool && d.set == set && d.accepted && d.source == m.source)
                .ok_or_else(|| format!("`{pool}:{set}` is not accepted from its source"))?;
            for n in names {
                if on {
                    d.off.remove(&n);
                } else {
                    d.off.insert(n);
                }
            }
            self.save(&s)?;
        }
        self.sync();
        Ok(())
    }

    /// What each skill name should be (pool, set, sha256), and the names
    /// two sets give.
    fn wanted(
        &self,
        s: &Settings,
        lists: &BTreeMap<String, Vec<SetManifest>>,
    ) -> (Wanted, BTreeSet<String>) {
        let mut want = Wanted::new();
        let mut twice = BTreeSet::new();
        for (pool, sets) in lists {
            for m in sets {
                let Some(d) = s.sets.iter().find(|d| {
                    &d.pool == pool && d.set == m.name && d.accepted && d.source == m.source
                }) else {
                    continue;
                };
                for k in m.skills.iter().filter(|k| !d.off.contains(&k.name)) {
                    let sha = if d.auto {
                        Some(k.sha256.clone())
                    } else {
                        d.approved.get(&k.name).cloned()
                    };
                    let Some(sha) = sha else { continue };
                    if want.contains_key(&k.name) {
                        twice.insert(k.name.clone());
                    } else {
                        want.insert(
                            k.name.clone(),
                            Want {
                                pool: pool.clone(),
                                set: m.name.clone(),
                                sha,
                                repos: k.repos.clone(),
                            },
                        );
                    }
                }
            }
        }
        for n in &twice {
            want.remove(n);
        }
        (want, twice)
    }

    /// The archive `sha256` of a skill: from the store, else downloaded
    /// and checked.
    fn archive(&self, pool: &str, set: &str, skill: &str, sha: &str) -> Result<Vec<u8>, String> {
        let store = self.dir.join("skill-store");
        let p = store.join(format!("{sha}.tar.gz"));
        if let Ok(b) = std::fs::read(&p)
            && devstack_node::skills::hex(&Sha256::digest(&b)) == sha
        {
            return Ok(b);
        }
        let b = (self.download)(pool, set, skill)?;
        let got = devstack_node::skills::hex(&Sha256::digest(&b));
        if got != sha {
            return Err(format!(
                "{skill}: the pool sent another version than it names (it changed meanwhile, or the download broke)"
            ));
        }
        let _ = std::fs::create_dir_all(&store);
        let _ = write(&p, &b);
        Ok(b)
    }

    /// Bring the agents' folders to what the accepted sets say: install,
    /// update, remove; leave alone what is not the hub's or was changed.
    pub fn sync(&self) {
        let _one = self.lock.lock().unwrap();
        let mut s = self.settings();
        let lists = self.lists();
        let (want, twice) = self.wanted(&s, &lists);
        let mut conflicts: Vec<Conflict> = twice
            .iter()
            .map(|n| Conflict {
                dir: String::new(),
                name: n.clone(),
                reason: "two accepted sets have a skill of this name: neither is installed".into(),
            })
            .collect();
        let mut errors = Vec::new();
        let trees = (self.trees)();
        *self.seen.lock().unwrap() = trees.clone();
        // The folders each skill goes to.
        let places: BTreeMap<&String, Vec<Place>> = want
            .iter()
            .map(|(n, w)| (n, self.places(&s, &w.repos, &trees)))
            .collect();
        // What goes: in a folder no longer written to, or no longer wanted
        // as it is there.
        let mut keep = Vec::new();
        for i in std::mem::take(&mut s.installed) {
            let wanted_here = want
                .get(&i.name)
                .is_some_and(|w| w.pool == i.pool && w.set == i.set)
                && places
                    .get(&i.name)
                    .is_some_and(|ps| ps.iter().any(|p| p.dir == i.dir));
            if wanted_here {
                keep.push(i);
                continue;
            }
            let path = i.dir.join(&i.name);
            match tree(&path) {
                Some(t) if t == i.tree => {
                    if let Err(e) = std::fs::remove_dir_all(&path) {
                        errors.push(format!("{}: {e}", path.display()));
                        keep.push(i);
                    }
                }
                Some(_) => conflicts.push(Conflict {
                    dir: i.dir.to_string_lossy().into_owned(),
                    name: i.name.clone(),
                    reason: "changed here, so left in place when its set no longer has it; it is yours now".into(),
                }),
                None => {}
            }
        }
        s.installed = keep;
        for (name, w) in &want {
            let (pool, set, sha) = (&w.pool, &w.set, &w.sha);
            for place in &places[name] {
                let dir = &place.dir;
                let path = dir.join(name);
                let mine = s
                    .installed
                    .iter()
                    .position(|i| &i.dir == dir && &i.name == name);
                match mine {
                    None if path.exists() => {
                        conflicts.push(Conflict {
                            dir: dir.to_string_lossy().into_owned(),
                            name: name.clone(),
                            reason: "a skill of this name is there already, not installed by devstack: left as it is".into(),
                        });
                        continue;
                    }
                    Some(i) => {
                        let now = tree(&path);
                        if now.as_deref() != Some(s.installed[i].tree.as_str()) {
                            if now.is_some() {
                                conflicts.push(Conflict {
                                    dir: dir.to_string_lossy().into_owned(),
                                    name: name.clone(),
                                    reason: "changed here since devstack installed it: not updated"
                                        .into(),
                                });
                                continue;
                            }
                        } else if &s.installed[i].sha256 == sha {
                            continue;
                        }
                    }
                    None => {}
                }
                // A file git tracks there is the repository's (ADR-0071).
                if let Some(root) = &place.tree
                    && let Ok(rel) = path.strip_prefix(root)
                    && (self.tracked)(root, rel)
                {
                    conflicts.push(Conflict {
                        dir: dir.to_string_lossy().into_owned(),
                        name: name.clone(),
                        reason: "git tracks a file of this skill in the working tree: devstack writes none of the repository's files".into(),
                    });
                    continue;
                }
                let result = self
                    .archive(pool, set, name, sha)
                    .and_then(|bytes| install(dir, name, &bytes, place.tree.is_some()));
                match result {
                    Ok(t) => {
                        s.installed.retain(|i| !(&i.dir == dir && &i.name == name));
                        s.installed.push(Installed {
                            dir: dir.clone(),
                            name: name.clone(),
                            pool: pool.clone(),
                            set: set.clone(),
                            sha256: sha.clone(),
                            tree: t,
                            updated: now_ms(),
                        });
                    }
                    Err(e) => errors.push(format!("{name} into {}: {e}", dir.display())),
                }
            }
        }
        let _ = self.save(&s);
        self.prune_store(&lists);
        *self.last.lock().unwrap() = (conflicts, errors, Some(now_ms()));
        drop(_one);
        self.publish();
    }

    /// The folders a skill bound to `repos` goes to: the user folders of the
    /// agents switched on, or, bound, their folders in the working trees of
    /// those repositories. Each once, with the agents that read it.
    fn places(&self, s: &Settings, repos: &[String], trees: &[Tree]) -> Vec<Place> {
        let mut out: Vec<Place> = Vec::new();
        let mut add = |dir: PathBuf, tree: Option<PathBuf>, agent: &str| match out
            .iter_mut()
            .find(|p| p.dir == dir)
        {
            Some(p) => p.agents.push(agent.to_string()),
            None => out.push(Place {
                dir,
                tree,
                agents: vec![agent.to_string()],
            }),
        };
        for a in devstack_skills::agents::all() {
            if s.agents.get(&a.id) != Some(&true) {
                continue;
            }
            if repos.is_empty() {
                if let Some(d) = (self.folder)(a) {
                    add(d, None, &a.name);
                }
                continue;
            }
            let Some(rel) = &a.project else { continue };
            for t in trees {
                if t.origin
                    .as_deref()
                    .is_some_and(|o| devstack_node::skills::repo_matches(repos, o))
                {
                    let mut d = t.root.clone();
                    d.extend(rel.split('/').filter(|p| !p.is_empty()));
                    add(d, Some(t.root.clone()), &a.name);
                }
            }
        }
        out
    }

    /// Archives no set names any more go from the store.
    fn prune_store(&self, lists: &BTreeMap<String, Vec<SetManifest>>) {
        let named: BTreeSet<String> = lists
            .values()
            .flatten()
            .flat_map(|m| m.skills.iter().map(|k| format!("{}.tar.gz", k.sha256)))
            .chain(
                self.settings()
                    .sets
                    .iter()
                    .flat_map(|d| d.approved.values().map(|v| format!("{v}.tar.gz"))),
            )
            .collect();
        if let Ok(rd) = std::fs::read_dir(self.dir.join("skill-store")) {
            for e in rd.flatten() {
                if !named.contains(&e.file_name().to_string_lossy().into_owned()) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    /// The state now.
    pub fn view(&self) -> View {
        let s = self.settings();
        let lists = self.lists();
        let agents = self.agents(&s);
        let trees = self.seen.lock().unwrap().clone();
        let mut sets = Vec::new();
        for (pool, ms) in &lists {
            for m in ms {
                let d = s.sets.iter().find(|d| &d.pool == pool && d.set == m.name);
                let state = match d {
                    None => "offered",
                    Some(d) if d.source != m.source => "changed",
                    Some(d) if d.accepted => "accepted",
                    Some(_) => "declined",
                };
                let accepted = state == "accepted";
                let auto = d.is_some_and(|d| d.auto);
                sets.push(SetView {
                    pool: pool.clone(),
                    set: m.name.clone(),
                    source: m.source.clone(),
                    state,
                    auto,
                    fetched: m.fetched,
                    commit: m.commit.clone(),
                    error: m.error.clone(),
                    skills: m
                        .skills
                        .iter()
                        .map(|k| {
                            let places = self.places(&s, &k.repos, &trees);
                            let enabled = !d.is_some_and(|d| d.off.contains(&k.name));
                            let mine = |i: &&Installed| {
                                &i.pool == pool && i.set == m.name && i.name == k.name
                            };
                            let here = |dir: &PathBuf| {
                                s.installed
                                    .iter()
                                    .filter(mine)
                                    .any(|i| &i.dir == dir && i.sha256 == k.sha256)
                            };
                            SkillView {
                                name: k.name.clone(),
                                description: k.description.clone(),
                                sha256: k.sha256.clone(),
                                installed: accepted
                                    && enabled
                                    && !places.is_empty()
                                    && places.iter().all(|p| here(&p.dir)),
                                pending: accepted
                                    && enabled
                                    && !auto
                                    && d.and_then(|d| d.approved.get(&k.name)) != Some(&k.sha256),
                                enabled,
                                group: k.group.clone(),
                                tags: k.tags.clone(),
                                updated: s
                                    .installed
                                    .iter()
                                    .filter(mine)
                                    .map(|i| i.updated)
                                    .filter(|u| *u > 0)
                                    .max(),
                                agents: agents
                                    .iter()
                                    .filter(|a| {
                                        places
                                            .iter()
                                            .any(|p| p.agents.contains(&a.name) && here(&p.dir))
                                    })
                                    .map(|a| a.name.clone())
                                    .collect(),
                                repos: k.repos.clone(),
                                trees: {
                                    let mut t: Vec<String> = places
                                        .iter()
                                        .filter(|p| here(&p.dir))
                                        .filter_map(|p| p.tree.as_ref())
                                        .map(|t| t.to_string_lossy().into_owned())
                                        .collect();
                                    t.sort();
                                    t.dedup();
                                    t
                                },
                            }
                        })
                        .collect(),
                });
            }
        }
        let (conflicts, errors, synced) = self.last.lock().unwrap().clone();
        View {
            agents,
            sets,
            conflicts,
            errors,
            synced,
        }
    }

    /// Look again which agents are on this machine (one installed since
    /// shows), and tell the dashboards: the Skills page asks when it opens.
    pub fn refresh(&self) -> View {
        let v = self.view();
        self.tx.send_replace(v.clone());
        v
    }

    fn publish(&self) {
        let v = self.view();
        self.tx.send_replace(v);
    }
}

/// Write `bytes` to `path` through a file beside it.
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!("{}.tmp", rand::random::<u32>()));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
}

/// Unpack a skill into `dir/name`: into a folder beside it first, then
/// swapped in; in a working tree (`ignored`) with a `.gitignore` that
/// ignores the folder. The hash of what was written.
fn install(dir: &Path, name: &str, bytes: &[u8], ignored: bool) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let n = rand::random::<u32>();
    let fresh = dir.join(format!(".{name}.devstack-new-{n:08x}"));
    let old = dir.join(format!(".{name}.devstack-old-{n:08x}"));
    let unpacked = devstack_node::skills::unpack(bytes, &fresh).and_then(|()| {
        if ignored {
            std::fs::write(fresh.join(".gitignore"), devstack_skills::IGNORE_ALL)
                .map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    });
    if let Err(e) = unpacked {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(e);
    }
    let target = dir.join(name);
    if target.exists() {
        std::fs::rename(&target, &old).map_err(|e| {
            let _ = std::fs::remove_dir_all(&fresh);
            format!("{}: {e}", target.display())
        })?;
    }
    if let Err(e) = std::fs::rename(&fresh, &target) {
        let _ = std::fs::rename(&old, &target);
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(format!("{}: {e}", target.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    tree(&target).ok_or_else(|| format!("{}: not readable after writing", target.display()))
}

/// Keep the skills of the working trees in step (ADR-0079): when a stack is
/// registered or forgotten, a worktree comes or goes or checks out another
/// branch, a stack file changes, or devstack is another version, the bound
/// skills of the pools and the generated ones are written again. Once a
/// minute at the latest.
pub fn follow(skills: std::sync::Arc<PoolSkills>) {
    let _ = std::thread::Builder::new()
        .name("tree-skills".into())
        .spawn(move || {
            let worktrees = crate::worktrees::subscribe();
            let mut last: Option<(u64, std::time::Instant)> = None;
            loop {
                let print = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    worktrees.borrow().hash(&mut h);
                    for e in devstack_daemon::registry::list() {
                        e.file.hash(&mut h);
                        std::fs::metadata(&e.file)
                            .and_then(|m| m.modified())
                            .ok()
                            .hash(&mut h);
                    }
                    h.finish()
                };
                let due = last.is_none_or(|(p, at)| {
                    p != print || at.elapsed() >= std::time::Duration::from_secs(60)
                });
                if due {
                    skills.sync();
                    skills.local();
                    last = Some((print, std::time::Instant::now()));
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        });
}

/// The working trees of the stacks registered here (their main working
/// trees and worktrees), with their `origin`, asked of git once per
/// repository.
fn registered_trees() -> Vec<Tree> {
    let mut out: Vec<Tree> = Vec::new();
    for e in devstack_daemon::registry::list() {
        let origin = crate::branches::origin(&e.repo);
        let mut roots = vec![e.repo.clone()];
        roots.extend(
            devstack_daemon::lanes::worktree_branches(&e.repo)
                .unwrap_or_default()
                .into_iter()
                .map(|(p, _)| p),
        );
        for root in roots {
            if !out
                .iter()
                .any(|t| devstack_daemon::lanes::same_tree(&t.root, &root))
            {
                out.push(Tree {
                    root,
                    origin: origin.clone(),
                });
            }
        }
    }
    out
}

// ------------------------------------------------------------------ routes

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Run `f` on the hub's skills off the async runtime; the state after it,
/// or `409` with why not.
async fn on(
    s: &crate::AppState,
    f: impl FnOnce(&PoolSkills) -> Result<(), String> + Send + 'static,
) -> Response {
    let skills = s.skills.clone();
    let r = tokio::task::spawn_blocking(move || f(&skills).map(|()| skills.view())).await;
    match r {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `GET /hub/skills`: the agents, the pools' sets, what is installed.
/// The agents are looked for again: one installed since shows.
pub async fn get(State(s): State<crate::AppState>) -> Response {
    let skills = s.skills.clone();
    match tokio::task::spawn_blocking(move || skills.refresh()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct OnBody {
    pub enabled: bool,
}

/// `PUT /hub/skills/agents/{id}` `{enabled}`: install into that agent's
/// folder, or no longer (what is there of the pools' goes).
pub async fn agent(
    State(s): State<crate::AppState>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<OnBody>,
) -> Response {
    on(&s, move |k| k.set_agent(&id, b.enabled)).await
}

#[derive(Deserialize, Default)]
pub struct AutoBody {
    #[serde(default)]
    pub auto: bool,
}

/// `POST /hub/skills/sets/{pool}/{set}/accept` `{auto}`.
pub async fn accept(
    State(s): State<crate::AppState>,
    UrlPath((pool, set)): UrlPath<(String, String)>,
    body: Option<Json<AutoBody>>,
) -> Response {
    let auto = body.map(|Json(b)| b.auto).unwrap_or_default();
    on(&s, move |k| k.accept(&pool, &set, auto)).await
}

/// `POST /hub/skills/sets/{pool}/{set}/decline`.
pub async fn decline(
    State(s): State<crate::AppState>,
    UrlPath((pool, set)): UrlPath<(String, String)>,
) -> Response {
    on(&s, move |k| k.decline(&pool, &set)).await
}

/// `PUT /hub/skills/sets/{pool}/{set}` `{auto}`.
pub async fn auto(
    State(s): State<crate::AppState>,
    UrlPath((pool, set)): UrlPath<(String, String)>,
    Json(b): Json<AutoBody>,
) -> Response {
    on(&s, move |k| k.set_auto(&pool, &set, b.auto)).await
}

/// `POST /hub/skills/sets/{pool}/{set}/update`: agree to its waiting
/// updates.
pub async fn update(
    State(s): State<crate::AppState>,
    UrlPath((pool, set)): UrlPath<(String, String)>,
) -> Response {
    on(&s, move |k| k.update(&pool, &set)).await
}

/// `POST /hub/skills/sync`: bring the folders to the sets again (a skill
/// removed by hand comes back).
pub async fn sync(State(s): State<crate::AppState>) -> Response {
    on(&s, |k| {
        k.sync();
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use devstack_node::skills::SkillEntry;
    use std::sync::Arc;

    /// A skill's archive with `text` in its `SKILL.md`.
    fn archive(name: &str, text: &str) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut t = tar::Builder::new(gz);
        let body = format!("---\nname: {name}\ndescription: d\n---\n{text}");
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        t.append_data(&mut h, "SKILL.md", body.as_bytes()).unwrap();
        t.into_inner().unwrap().finish().unwrap()
    }

    struct Pool {
        archives: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    }

    impl Pool {
        fn put(&self, name: &str, text: &str) -> SkillEntry {
            let b = archive(name, text);
            let e = SkillEntry {
                name: name.into(),
                description: "d".into(),
                sha256: devstack_node::skills::hex(&Sha256::digest(&b)),
                size: b.len() as u64,
                group: name.starts_with("biz").then(|| "Business".to_string()),
                tags: Vec::new(),
                repos: if name.starts_with("shop") {
                    vec!["acme/shop".into()]
                } else {
                    Vec::new()
                },
                folder: None,
            };
            self.archives.lock().unwrap().insert(name.into(), b);
            e
        }
    }

    #[test]
    fn a_bound_skill_goes_into_the_working_trees_of_its_repository_only() {
        let (st, home, wt) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let (mut hub, pool) = hub(st.path(), home.path());
        let (shop, lane, other) = (
            wt.path().join("shop"),
            wt.path().join("shop-lane"),
            wt.path().join("blog"),
        );
        let tree = |root: &Path, origin: &str| Tree {
            root: root.to_path_buf(),
            origin: Some(origin.into()),
        };
        let trees = Arc::new(Mutex::new(vec![
            tree(&shop, "git@github.com:acme/shop.git"),
            tree(&lane, "https://github.com/Acme/Shop"),
            tree(&other, "git@github.com:acme/blog.git"),
        ]));
        let t = trees.clone();
        hub.trees = Box::new(move || t.lock().unwrap().clone());
        let a = pool.put("work-issue", "v1");
        let b = pool.put("shop-page", "v1");
        hub.set_pool("default", Some(set(vec![a, b.clone()])));
        hub.set_agent("claude-code", true).unwrap();
        hub.set_agent("codex", true).unwrap();
        hub.accept("default", "core", true).unwrap();

        // Unbound: the user folders. Bound: the agents' folders of the
        // working trees of acme/shop, each ignoring itself.
        assert!(home.path().join(".claude/skills/work-issue").exists());
        assert!(!home.path().join(".claude/skills/shop-page").exists());
        for root in [&shop, &lane] {
            for d in [".claude/skills", ".agents/skills"] {
                let k = root.join(d).join("shop-page");
                assert!(k.join("SKILL.md").exists(), "{}", k.display());
                assert_eq!(
                    std::fs::read_to_string(k.join(".gitignore")).unwrap(),
                    devstack_skills::IGNORE_ALL
                );
            }
            assert!(!root.join(".claude/skills/work-issue").exists());
        }
        assert!(!other.join(".claude").exists());
        let v = hub.view();
        let k = v.sets[0]
            .skills
            .iter()
            .find(|k| k.name == "shop-page")
            .unwrap();
        assert!(k.installed, "{k:?}");
        assert_eq!(k.repos, ["acme/shop"]);
        assert_eq!(k.trees.len(), 2);
        assert_eq!(k.agents, ["Claude Code", "Codex"]);

        // A worktree removed: its folders are no longer the hub's.
        trees.lock().unwrap().retain(|t| t.root != lane);
        std::fs::remove_dir_all(&lane).unwrap();
        hub.sync();
        assert_eq!(hub.view().sets[0].skills[1].trees.len(), 1);

        // A file git tracks there is the repository's: not written over.
        hub.tracked = Box::new(|_, rel| rel.starts_with(".agents"));
        std::fs::remove_dir_all(shop.join(".agents/skills/shop-page")).unwrap();
        hub.sync();
        assert!(!shop.join(".agents/skills/shop-page").exists());
        assert!(
            hub.view()
                .conflicts
                .iter()
                .any(|c| c.name == "shop-page" && c.reason.contains("git tracks"))
        );
        hub.tracked = Box::new(|_, _| false);

        // A skill bound later leaves the user folders for the working trees.
        let mut bound = pool.put("work-issue", "v1");
        bound.repos = vec!["acme/blog".into()];
        hub.set_pool("default", Some(set(vec![bound, b])));
        assert!(!home.path().join(".claude/skills/work-issue").exists());
        assert!(other.join(".claude/skills/work-issue/SKILL.md").exists());
    }

    /// A hub whose agents `claude-code` and `codex` are found, with their
    /// folders in `home`.
    fn hub(state: &Path, home: &Path) -> (PoolSkills, Pool) {
        let archives: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::default();
        let a = archives.clone();
        let home = home.to_path_buf();
        let h2 = home.clone();
        let s = PoolSkills::with(
            state.to_path_buf(),
            Box::new(move |a| match a.id.as_str() {
                "claude-code" => Some(home.join(".claude").join("skills")),
                "codex" | "cline" => Some(home.join(".agents").join("skills")),
                "opencode" => Some(home.join(".config").join("opencode").join("skills")),
                _ => None,
            }),
            Box::new(move |a| {
                a.id == "claude-code"
                    || a.id == "codex"
                    || (a.id == "opencode" && h2.join(".config").join("opencode").exists())
            }),
            Box::new(move |_, _, skill| {
                a.lock()
                    .unwrap()
                    .get(skill)
                    .cloned()
                    .ok_or_else(|| "gone".into())
            }),
        );
        (s, Pool { archives })
    }

    fn set(skills: Vec<SkillEntry>) -> Vec<SetManifest> {
        vec![SetManifest {
            name: "core".into(),
            source: "github:org/skills@main".into(),
            skills,
            ..Default::default()
        }]
    }

    #[test]
    fn an_accepted_set_is_installed_into_the_agents_switched_on() {
        let (st, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hub, pool) = hub(st.path(), home.path());
        let a = pool.put("work-issue", "v1");
        hub.set_pool("default", Some(set(vec![a])));
        let claude = home.path().join(".claude/skills/work-issue/SKILL.md");
        // Offered only.
        assert_eq!(hub.view().sets[0].state, "offered");
        hub.set_agent("claude-code", true).unwrap();
        assert!(!claude.exists());
        hub.accept("default", "core", true).unwrap();
        assert!(std::fs::read_to_string(&claude).unwrap().ends_with("v1"));
        assert!(!home.path().join(".agents/skills/work-issue").exists());
        hub.set_agent("codex", true).unwrap();
        assert_eq!(
            hub.view().sets[0].skills[0].agents,
            ["Claude Code", "Codex"]
        );
        assert!(
            home.path()
                .join(".agents/skills/work-issue/SKILL.md")
                .exists()
        );
        assert!(hub.view().sets[0].skills[0].installed);

        // An update comes at once; switched off, the folder is cleared.
        let b = pool.put("work-issue", "v2");
        hub.set_pool("default", Some(set(vec![b])));
        assert!(std::fs::read_to_string(&claude).unwrap().ends_with("v2"));
        hub.set_agent("codex", false).unwrap();
        assert!(!home.path().join(".agents/skills/work-issue").exists());

        // The set gone from the pool: its skill goes.
        hub.set_pool("default", Some(Vec::new()));
        assert!(!claude.exists());
    }

    #[test]
    fn an_agent_installed_since_shows_when_the_page_asks() {
        let (st, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hub, _pool) = hub(st.path(), home.path());
        let rx = hub.subscribe();
        let ids = |v: &View| v.agents.iter().map(|a| a.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&rx.borrow()), ["claude-code", "codex"]);
        std::fs::create_dir_all(home.path().join(".config/opencode")).unwrap();
        hub.refresh();
        assert_eq!(ids(&rx.borrow()), ["claude-code", "codex", "opencode"]);
        assert!(
            !rx.borrow().agents[2].enabled,
            "a new agent is off until switched on"
        );
    }

    #[test]
    fn an_update_waits_for_the_users_word_when_asked_to() {
        let (st, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hub, pool) = hub(st.path(), home.path());
        let a = pool.put("work-issue", "v1");
        hub.set_pool("default", Some(set(vec![a.clone()])));
        hub.set_agent("claude-code", true).unwrap();
        hub.accept("default", "core", false).unwrap();
        let claude = home.path().join(".claude/skills/work-issue/SKILL.md");
        let b = pool.put("work-issue", "v2");
        // The store keeps v1, which the pool no longer sends.
        hub.set_pool("default", Some(set(vec![b])));
        assert!(std::fs::read_to_string(&claude).unwrap().ends_with("v1"));
        assert!(hub.view().sets[0].skills[0].pending);
        hub.update("default", "core").unwrap();
        assert!(std::fs::read_to_string(&claude).unwrap().ends_with("v2"));
        assert!(!hub.view().sets[0].skills[0].pending);
    }

    #[test]
    fn a_skill_of_the_users_own_or_one_changed_by_hand_stays() {
        let (st, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hub, pool) = hub(st.path(), home.path());
        let own = home.path().join(".claude/skills/push-tag");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("SKILL.md"), "mine").unwrap();
        let a = pool.put("work-issue", "v1");
        let p = pool.put("push-tag", "pool's");
        hub.set_pool("default", Some(set(vec![a, p])));
        hub.set_agent("claude-code", true).unwrap();
        hub.accept("default", "core", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(own.join("SKILL.md")).unwrap(),
            "mine"
        );
        let v = hub.view();
        assert!(
            v.conflicts.iter().any(|c| c.name == "push-tag"),
            "{:?}",
            v.conflicts
        );

        let claude = home.path().join(".claude/skills/work-issue/SKILL.md");
        std::fs::write(&claude, "edited").unwrap();
        let b = pool.put("work-issue", "v2");
        hub.set_pool("default", Some(set(vec![b])));
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), "edited");
        assert!(hub.view().conflicts.iter().any(|c| c.name == "work-issue"));
    }

    #[test]
    fn a_skill_or_a_group_is_switched_off_and_on_again() {
        let (st, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hub, pool) = hub(st.path(), home.path());
        let a = pool.put("work-issue", "v1");
        let b = pool.put("biz-validate", "v1");
        let c = pool.put("biz-overview", "v1");
        hub.set_pool("default", Some(set(vec![a, b, c])));
        hub.set_agent("claude-code", true).unwrap();
        hub.accept("default", "core", true).unwrap();
        let dir = home.path().join(".claude/skills");
        let v = hub.view();
        let k = &v.sets[0].skills[0];
        assert!(k.enabled && k.installed && k.updated.is_some(), "{k:?}");
        assert_eq!(k.agents, ["Claude Code"]);

        hub.set_skill("default", "core", "work-issue", false)
            .unwrap();
        assert!(!dir.join("work-issue").exists());
        let v = hub.view();
        let k = v.sets[0]
            .skills
            .iter()
            .find(|k| k.name == "work-issue")
            .unwrap();
        assert!(!k.enabled && !k.installed);

        hub.set_group("default", "core", "Business", false).unwrap();
        assert!(!dir.join("biz-validate").exists() && !dir.join("biz-overview").exists());
        hub.set_group("default", "core", "Business", true).unwrap();
        hub.set_skill("default", "core", "work-issue", true)
            .unwrap();
        for n in ["work-issue", "biz-validate", "biz-overview"] {
            assert!(dir.join(n).join("SKILL.md").exists(), "{n}");
        }
        // A skill the set gains later is on.
        let d = pool.put("push-tag", "v1");
        let all: Vec<_> = hub.view().sets[0]
            .skills
            .iter()
            .map(|k| k.name.clone())
            .collect();
        assert_eq!(all.len(), 3);
        let mut entries = set(vec![d]);
        entries[0]
            .skills
            .extend(["work-issue", "biz-validate", "biz-overview"].map(|n| pool.put(n, "v1")));
        hub.set_pool("default", Some(entries));
        assert!(dir.join("push-tag").exists());
    }

    #[test]
    fn a_set_from_another_source_is_offered_again() {
        let (st, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (hub, pool) = hub(st.path(), home.path());
        let a = pool.put("work-issue", "v1");
        hub.set_pool("default", Some(set(vec![a.clone()])));
        hub.set_agent("claude-code", true).unwrap();
        hub.accept("default", "core", true).unwrap();
        let mut moved = set(vec![a]);
        moved[0].source = "github:someone/else@main".into();
        hub.set_pool("default", Some(moved));
        assert_eq!(hub.view().sets[0].state, "changed");
        assert!(!home.path().join(".claude/skills/work-issue").exists());
    }
}

#[derive(Deserialize)]
pub struct EnabledBody {
    pub enabled: bool,
}

/// `PUT /hub/skills/sets/{pool}/{set}/skills/{name}` `{enabled}`: switch
/// one skill of an accepted set on or off (ADR-0077).
pub async fn skill(
    State(s): State<crate::AppState>,
    UrlPath((pool, set, name)): UrlPath<(String, String, String)>,
    Json(b): Json<EnabledBody>,
) -> Response {
    on(&s, move |k| k.set_skill(&pool, &set, &name, b.enabled)).await
}

/// `PUT /hub/skills/sets/{pool}/{set}/groups/{group}` `{enabled}`: switch
/// every skill of a group on or off; `-` is the skills without one.
pub async fn group(
    State(s): State<crate::AppState>,
    UrlPath((pool, set, group)): UrlPath<(String, String, String)>,
    Json(b): Json<EnabledBody>,
) -> Response {
    let group = if group == "-" { String::new() } else { group };
    on(&s, move |k| k.set_group(&pool, &set, &group, b.enabled)).await
}
