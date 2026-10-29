//! The skill sets a pool hands out (ADR-0074): agent skills (folders with a
//! `SKILL.md`) the coordinator fetches from a source with its own
//! credentials and packs, one archive per skill, for the members a set is
//! given to. Each member's hub installs the sets its user accepted into the
//! agents' user skill folders.
//!
//! - **Sources**: a GitHub repository (its archive over the API, with the
//!   read token in `DEVSTACK_SKILLS_GITHUB_TOKEN` when there is one), or a
//!   folder below `skill-sources/` in the coordinator's directory. Members
//!   never reach the source.
//! - **Given to** (`to`): `*`, member ids or tags; a member reads and
//!   downloads only the sets given to it.
//! - **Fetched** when the coordinator starts, when an admin changes or
//!   refreshes a set, and when the source says it changed
//!   (`POST /v1/skills/refresh`, signed). Never on a timer.
//! - Kept in `skills.json` (the sets) and `skills/<set>/` (the manifest and
//!   the archives of the last fetch that worked) in the coordinator's
//!   directory.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const FILE: &str = "skills.json";
const CACHE: &str = "skills";
/// Where folder sources are, in the coordinator's directory.
pub const SOURCES: &str = "skill-sources";
/// The variable with a read token for GitHub sources.
pub const GITHUB_TOKEN: &str = "DEVSTACK_SKILLS_GITHUB_TOKEN";
/// The variable with the secret a refresh is signed with.
pub const REFRESH_SECRET: &str = "DEVSTACK_SKILLS_REFRESH_SECRET";

/// At most this many sets in a pool.
pub const MAX_SETS: usize = 64;
/// A skill's archive at most.
pub const MAX_ARCHIVE: u64 = 10 << 20;
/// A skill's files together at most, unpacked.
pub const MAX_UNPACKED: u64 = 25 << 20;
/// A skill's files at most.
pub const MAX_FILES: usize = 1000;
/// A source's download at most (a repository's archive).
const MAX_SOURCE: u64 = 200 << 20;

/// A set as an admin names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSet {
    /// `[a-z0-9-]`, 1 to 32.
    pub name: String,
    pub source: Source,
    /// Whom it is given to: `*`, member ids, tags.
    #[serde(default)]
    pub to: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// `owner/name` at `ref` (a branch, tag or commit); the skills are the
    /// folders in `path` (`""`: the top).
    Github {
        repo: String,
        #[serde(rename = "ref", default = "main")]
        reference: String,
        #[serde(default)]
        path: String,
    },
    /// A folder below `skill-sources/` in the coordinator's directory.
    Folder { path: String },
}

fn main() -> String {
    "main".into()
}

impl Source {
    /// What a member accepts: a set whose source says otherwise is asked
    /// again.
    pub fn label(&self) -> String {
        match self {
            Source::Github {
                repo,
                reference,
                path,
            } => {
                let p = path.trim_matches('/');
                if p.is_empty() {
                    format!("github:{repo}@{reference}")
                } else {
                    format!("github:{repo}@{reference}/{p}")
                }
            }
            Source::Folder { path } => format!("folder:{}", path.trim_matches('/')),
        }
    }
}

fn relative(p: &str) -> bool {
    let p = p.trim_matches('/');
    p.is_empty()
        || (!p.contains('\\')
            && Path::new(p)
                .components()
                .all(|c| matches!(c, Component::Normal(_))))
}

impl SkillSet {
    /// What is wrong with it, if anything.
    pub fn check(&self) -> Result<(), String> {
        let n = &self.name;
        if n.is_empty()
            || n.len() > 32
            || !n
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!("`{n}`: a set's name is 1 to 32 of a-z, 0-9 and -"));
        }
        match &self.source {
            Source::Github {
                repo,
                reference,
                path,
            } => {
                let ok = |s: &str| {
                    !s.is_empty()
                        && s.len() <= 100
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                };
                match repo.split_once('/') {
                    Some((o, r)) if ok(o) && ok(r) && r != "." && r != ".." => {}
                    _ => return Err(format!("`{repo}`: a GitHub repository is owner/name")),
                }
                if reference.is_empty()
                    || reference.len() > 255
                    || reference
                        .bytes()
                        .any(|b| b.is_ascii_whitespace() || b == b'?' || b == b'#')
                    || reference.contains("..")
                {
                    return Err(format!("`{reference}`: not a branch, tag or commit"));
                }
                if !relative(path) {
                    return Err(format!("`{path}`: a path in the repository, without `..`"));
                }
            }
            Source::Folder { path } => {
                if path.trim_matches('/').is_empty() || !relative(path) {
                    return Err(format!(
                        "`{path}`: a folder below `{SOURCES}/` in the coordinator's directory, without `..`"
                    ));
                }
            }
        }
        if self.to.len() > 256 || self.to.iter().any(|t| t.is_empty() || t.len() > 64) {
            return Err("`to` names 1 to 64 characters each, 256 at most".into());
        }
        Ok(())
    }

    /// Whether the member `id` with `tags` gets it.
    pub fn given_to(&self, id: &str, tags: &[String]) -> bool {
        self.to
            .iter()
            .any(|t| t == "*" || t == id || tags.iter().any(|x| x == t))
    }
}

/// One skill of a set, as the last fetch found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    /// Of its archive.
    pub sha256: String,
    pub size: u64,
    /// `metadata.group` of its frontmatter (ADR-0077); absent from an older
    /// coordinator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// `metadata.tags` of its frontmatter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// A set as its members read it: what the last fetch that worked found,
/// and why the last one failed, if it did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetManifest {
    pub name: String,
    /// The source's label (`Source::label`).
    pub source: String,
    /// When it was fetched (Unix s); `None`: never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched: Option<u64>,
    /// The commit the archive was of, for a GitHub source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub skills: Vec<SkillEntry>,
    /// Folders left out, and why (no `SKILL.md`, a wrong name, too large).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// A set with its state, as admins see it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetView {
    #[serde(flatten)]
    pub set: SkillSet,
    pub manifest: SetManifest,
}

/// The admin view: the sets, and what the coordinator is set up with.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminView {
    pub sets: Vec<SetView>,
    /// A GitHub token is set (`DEVSTACK_SKILLS_GITHUB_TOKEN`).
    pub github_token: bool,
    /// A signed refresh is taken (`DEVSTACK_SKILLS_REFRESH_SECRET`).
    pub refresh_signed: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    sets: Vec<SkillSet>,
}

/// The sets in the coordinator's `dir`.
pub fn load(dir: &Path) -> Vec<SkillSet> {
    std::fs::read(dir.join(FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<File>(&b).ok())
        .map(|f| f.sets)
        .unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", crate::coordinator::random_hex(4)));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

fn save(dir: &Path, sets: &[SkillSet]) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(&File {
        sets: sets.to_vec(),
    })
    .expect("the sets serialise");
    write_atomic(&dir.join(FILE), &bytes)
}

/// Add `s` to the sets in `dir`, or replace the one of its name. `Err` with
/// why not, an I/O error as `Ok(Err)`.
pub fn put(dir: &Path, s: SkillSet) -> Result<std::io::Result<()>, String> {
    s.check()?;
    let mut all = load(dir);
    let full = all.len() >= MAX_SETS;
    match all.iter_mut().find(|o| o.name == s.name) {
        Some(o) => *o = s,
        None if full => return Err(format!("a pool hands out {MAX_SETS} sets at most")),
        None => all.push(s),
    }
    Ok(save(dir, &all))
}

/// Remove the set `name` and what was fetched for it; whether it was there.
pub fn remove(dir: &Path, name: &str) -> std::io::Result<bool> {
    let mut all = load(dir);
    let before = all.len();
    all.retain(|s| s.name != name);
    if all.len() == before {
        return Ok(false);
    }
    save(dir, &all)?;
    let _ = std::fs::remove_dir_all(dir.join(CACHE).join(name));
    Ok(true)
}

fn cache(dir: &Path, set: &str) -> PathBuf {
    dir.join(CACHE).join(set)
}

/// The manifest of the set `s` as last fetched.
pub fn manifest(dir: &Path, s: &SkillSet) -> SetManifest {
    std::fs::read(cache(dir, &s.name).join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<SetManifest>(&b).ok())
        .unwrap_or_else(|| SetManifest {
            name: s.name.clone(),
            source: s.source.label(),
            ..Default::default()
        })
}

/// The sets given to the member `id` with `tags`, as last fetched.
pub fn for_member(dir: &Path, id: &str, tags: &[String]) -> Vec<SetManifest> {
    load(dir)
        .iter()
        .filter(|s| s.given_to(id, tags))
        .map(|s| manifest(dir, s))
        .collect()
}

/// Every set with its state.
pub fn admin_view(dir: &Path) -> AdminView {
    AdminView {
        sets: load(dir)
            .into_iter()
            .map(|s| SetView {
                manifest: manifest(dir, &s),
                set: s,
            })
            .collect(),
        github_token: std::env::var(GITHUB_TOKEN).is_ok_and(|t| !t.trim().is_empty()),
        refresh_signed: refresh_secret().is_some(),
    }
}

/// The archive of `skill` in the set `set`, if the last fetch made one.
pub fn archive(dir: &Path, set: &str, skill: &str) -> Option<Vec<u8>> {
    if !valid_skill_name(skill) {
        return None;
    }
    std::fs::read(cache(dir, set).join(format!("{skill}.tar.gz"))).ok()
}

/// A skill's name as the Agent Skills format has it: 1 to 64 of a-z, 0-9
/// and `-`, not starting or ending with `-`.
pub fn valid_skill_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('-')
        && !n.ends_with('-')
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The files of the skills a source holds: by skill folder, by path in it.
type Found = BTreeMap<String, BTreeMap<String, (Vec<u8>, bool)>>;

/// Fetch the set `s` in `dir` again: its skills packed, its manifest
/// written. A fetch that fails keeps what the last one that worked made,
/// with the error in the manifest.
pub fn refresh(dir: &Path, s: &SkillSet) -> SetManifest {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let old = manifest(dir, s);
    let fetched = match &s.source {
        Source::Github {
            repo,
            reference,
            path,
        } => fetch_github(repo, reference, path),
        Source::Folder { path } => {
            read_folder(&dir.join(SOURCES).join(path.trim_matches('/'))).map(|f| (f, None))
        }
    };
    let target = cache(dir, &s.name);
    let m = match fetched
        .and_then(|(found, commit)| pack(&target, &s.name, &s.source.label(), found, commit, now))
    {
        Ok(m) => m,
        Err(e) => SetManifest {
            name: s.name.clone(),
            error: Some(e),
            // What members have stays theirs to install, unless the source
            // is another one now.
            skills: if old.source == s.source.label() {
                old.skills
            } else {
                Vec::new()
            },
            source: s.source.label(),
            fetched: old.fetched,
            commit: old.commit,
            skipped: old.skipped,
        },
    };
    let _ = std::fs::create_dir_all(&target);
    let _ = write_atomic(
        &target.join("manifest.json"),
        &serde_json::to_vec_pretty(&m).expect("a manifest serialises"),
    );
    m
}

fn pack(
    target: &Path,
    set: &str,
    source: &str,
    found: Found,
    commit: Option<String>,
    now: u64,
) -> Result<SetManifest, String> {
    let mut m = SetManifest {
        name: set.to_string(),
        source: source.to_string(),
        fetched: Some(now),
        commit,
        ..Default::default()
    };
    let mut archives = Vec::new();
    for (folder, files) in found {
        match skill(&folder, &files) {
            Ok(front) => {
                let bytes = tar_gz(&files).map_err(|e| format!("{folder}: {e}"))?;
                if bytes.len() as u64 > MAX_ARCHIVE {
                    m.skipped.push(format!(
                        "{folder}: its archive is over {} MiB",
                        MAX_ARCHIVE >> 20
                    ));
                    continue;
                }
                m.skills.push(SkillEntry {
                    name: folder.clone(),
                    description: front.description,
                    sha256: hex(&Sha256::digest(&bytes)),
                    size: bytes.len() as u64,
                    group: front.group,
                    tags: front.tags,
                });
                archives.push((folder, bytes));
            }
            Err(e) => m.skipped.push(format!("{folder}: {e}")),
        }
    }
    if m.skills.is_empty() && !m.skipped.is_empty() {
        return Err(format!(
            "no skill there to hand out: {}",
            m.skipped.join("; ")
        ));
    }
    // A new folder, then swapped in: members never read half a fetch.
    let parent = target.parent().ok_or("no cache folder")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let fresh = parent.join(format!(".{set}.{}", crate::coordinator::random_hex(4)));
    std::fs::create_dir_all(&fresh).map_err(|e| e.to_string())?;
    for (name, bytes) in &archives {
        std::fs::write(fresh.join(format!("{name}.tar.gz")), bytes).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        fresh.join("manifest.json"),
        serde_json::to_vec_pretty(&m).expect("a manifest serialises"),
    )
    .map_err(|e| e.to_string())?;
    let old = parent.join(format!(".{set}.old.{}", crate::coordinator::random_hex(4)));
    if target.exists() {
        std::fs::rename(target, &old).map_err(|e| e.to_string())?;
    }
    if let Err(e) = std::fs::rename(&fresh, target) {
        let _ = std::fs::rename(&old, target);
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(e.to_string());
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(m)
}

/// Whether `files` are a skill named `folder`: its frontmatter, or why not.
fn skill(folder: &str, files: &BTreeMap<String, (Vec<u8>, bool)>) -> Result<Front, String> {
    if !valid_skill_name(folder) {
        return Err("a skill's folder is 1 to 64 of a-z, 0-9 and -".into());
    }
    let Some((md, _)) = files.get("SKILL.md") else {
        return Err("no SKILL.md".into());
    };
    if files.len() > MAX_FILES {
        return Err(format!("over {MAX_FILES} files"));
    }
    let total: u64 = files.values().map(|(b, _)| b.len() as u64).sum();
    if total > MAX_UNPACKED {
        return Err(format!("over {} MiB unpacked", MAX_UNPACKED >> 20));
    }
    let front = frontmatter(&String::from_utf8_lossy(md))?;
    if front.name != folder {
        return Err(format!(
            "SKILL.md names `{}`, its folder is `{folder}`",
            front.name
        ));
    }
    if front.description.is_empty() || front.description.len() > 1024 {
        return Err("its description is 1 to 1024 characters".into());
    }
    Ok(front)
}

/// What a `SKILL.md`'s frontmatter says: `name`, `description`, and from
/// its `metadata` (the Agent Skills format's free map) `group` and `tags`,
/// which group and filter the skills of a set (ADR-0077).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Front {
    pub name: String,
    pub description: String,
    pub group: Option<String>,
    pub tags: Vec<String>,
}

/// The frontmatter of a `SKILL.md`.
pub fn frontmatter(md: &str) -> Result<Front, String> {
    enum Block {
        None,
        /// A folded or literal description, its lines so far.
        Description(Vec<String>),
        /// The `metadata` map, and its key last seen (a `tags` list may
        /// follow on the lines after it).
        Metadata(String),
    }
    let md = md.strip_prefix('\u{feff}').unwrap_or(md);
    let mut lines = md.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("SKILL.md starts without frontmatter (`---`)".into());
    }
    let (mut name, mut description) = (None, None);
    let (mut group, mut tags) = (None, Vec::new());
    let mut block = Block::None;
    let list = |v: &str| -> Vec<String> {
        v.trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(unquote)
            .filter(|t| !t.is_empty())
            .collect()
    };
    for l in lines {
        if l.trim() == "---" {
            break;
        }
        let indented = l.starts_with(' ') || l.starts_with('\t');
        match &mut block {
            Block::Description(acc) if indented || l.trim().is_empty() => {
                acc.push(l.trim().to_string());
                continue;
            }
            Block::Description(acc) => {
                description = Some(acc.join(" ").trim().to_string());
                block = Block::None;
            }
            Block::Metadata(last) if indented => {
                let t = l.trim();
                if let Some(item) = t.strip_prefix("- ") {
                    if last == "tags" {
                        tags.push(unquote(item));
                    }
                } else if let Some((k, v)) = t.split_once(':') {
                    let (k, v) = (k.trim(), v.trim());
                    *last = k.to_string();
                    match k {
                        "group" if !v.is_empty() => group = Some(unquote(v)),
                        "tags" if !v.is_empty() => tags = list(v),
                        _ => {}
                    }
                }
                continue;
            }
            Block::Metadata(_) => block = Block::None,
            Block::None => {}
        }
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "name" => name = Some(unquote(v)),
            "description" if v.is_empty() || v == ">" || v == "|" || v == ">-" || v == "|-" => {
                block = Block::Description(Vec::new());
            }
            "description" => description = Some(unquote(v)),
            "metadata" if v.is_empty() => block = Block::Metadata(String::new()),
            _ => {}
        }
    }
    if let Block::Description(acc) = block {
        description = Some(acc.join(" ").trim().to_string());
    }
    let group = group.filter(|g| !g.is_empty() && g.chars().count() <= 48);
    tags.retain(|t| t.chars().count() <= 32);
    tags.truncate(16);
    Ok(Front {
        name: name.ok_or("SKILL.md names no `name`")?,
        description: description.ok_or("SKILL.md has no `description`")?,
        group,
        tags,
    })
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        v[1..v.len() - 1].replace("\\\"", "\"").replace("''", "'")
    } else {
        v.to_string()
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A skill's files as a `.tar.gz`, the same bytes for the same files.
fn tar_gz(files: &BTreeMap<String, (Vec<u8>, bool)>) -> std::io::Result<Vec<u8>> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut t = tar::Builder::new(gz);
    for (path, (bytes, exec)) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(if *exec { 0o755 } else { 0o644 });
        h.set_mtime(0);
        h.set_entry_type(tar::EntryType::Regular);
        t.append_data(&mut h, path, bytes.as_slice())?;
    }
    t.into_inner()?.finish()
}

/// Unpack a skill's `.tar.gz` into `into` (made new): regular files only,
/// paths without `..`, within the limits. What a hub installs.
pub fn unpack(bytes: &[u8], into: &Path) -> Result<(), String> {
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let (mut n, mut total) = (0usize, 0u64);
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    for e in a.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| e.to_string())?;
        if e.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let p = e.path().map_err(|e| e.to_string())?.into_owned();
        if !p.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!("{}: not a path inside the skill", p.display()));
        }
        n += 1;
        total += e.header().size().unwrap_or(0);
        if n > MAX_FILES || total > MAX_UNPACKED {
            return Err("over the limits of a skill".into());
        }
        let to = into.join(&p);
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        std::fs::write(&to, &buf).map_err(|e| format!("{}: {e}", to.display()))?;
        #[cfg(unix)]
        if e.header().mode().is_ok_and(|m| m & 0o111 != 0) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755));
        }
    }
    Ok(())
}

/// The skill folders directly in `root`, each with its files.
fn read_folder(root: &Path) -> Result<Found, String> {
    let entries = std::fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut out = Found::new();
    for e in entries.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if !t.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let mut files = BTreeMap::new();
        walk(&e.path(), "", &mut files)?;
        out.insert(name, files);
    }
    Ok(out)
}

fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, (Vec<u8>, bool)>) -> Result<(), String> {
    for e in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let Ok(t) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if t.is_dir() {
            walk(&e.path(), &path, out)?;
        } else if t.is_file() {
            if out.len() > MAX_FILES {
                return Ok(());
            }
            let bytes = std::fs::read(e.path()).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            let exec = {
                use std::os::unix::fs::PermissionsExt;
                e.metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
            };
            #[cfg(not(unix))]
            let exec = false;
            out.insert(path, (bytes, exec));
        }
    }
    Ok(())
}

/// The skills below `path` in a repository's `.tar.gz` archive as GitHub
/// sends it (one folder on top, `<owner>-<repo>-<commit>/`), and the commit.
pub fn from_github_archive(bytes: &[u8], path: &str) -> Result<(Found, Option<String>), String> {
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let path = path.trim_matches('/');
    let mut out = Found::new();
    let mut commit = None;
    for e in a.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| e.to_string())?;
        // GitHub's archive starts with a pax global header (the commit in
        // its comment), named `pax_global_header`: not the top folder.
        if !matches!(
            e.header().entry_type(),
            tar::EntryType::Regular | tar::EntryType::Directory
        ) {
            continue;
        }
        let p = e.path().map_err(|e| e.to_string())?.into_owned();
        let parts: Vec<String> = p
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        if parts.len() != p.components().count() || parts.is_empty() {
            continue;
        }
        if commit.is_none() {
            commit = parts[0].rsplit('-').next().map(str::to_string);
        }
        if e.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let inside = &parts[1..];
        let base: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if inside.len() < base.len() + 2 || inside[..base.len()] != base[..] {
            continue;
        }
        let rest = &inside[base.len()..];
        let files = out.entry(rest[0].clone()).or_default();
        if files.len() > MAX_FILES {
            continue;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        let exec = e.header().mode().is_ok_and(|m| m & 0o111 != 0);
        files.insert(rest[1..].join("/"), (buf, exec));
    }
    Ok((out, commit))
}

/// Where GitHub's API is; another one for tests.
fn github_api() -> String {
    std::env::var("DEVSTACK_SKILLS_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".into())
}

fn fetch_github(
    repo: &str,
    reference: &str,
    path: &str,
) -> Result<(Found, Option<String>), String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(120)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let url = format!("{}/repos/{repo}/tarball/{reference}", github_api());
    let mut req = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header(
            "User-Agent",
            concat!("devstack/", env!("CARGO_PKG_VERSION")),
        );
    if let Ok(t) = std::env::var(GITHUB_TOKEN)
        && !t.trim().is_empty()
    {
        req = req.header("Authorization", &format!("Bearer {}", t.trim()));
    }
    let mut res = req.call().map_err(|e| format!("GitHub: {e}"))?;
    let status = res.status().as_u16();
    if status != 200 {
        return Err(match status {
            404 => format!(
                "GitHub: no `{repo}` at `{reference}` (or no access: a private repository needs {GITHUB_TOKEN} on the coordinator)"
            ),
            401 | 403 => format!("GitHub refused ({status}): check {GITHUB_TOKEN}"),
            s => format!("GitHub answered {s}"),
        });
    }
    let bytes = res
        .body_mut()
        .with_config()
        .limit(MAX_SOURCE)
        .read_to_vec()
        .map_err(|e| format!("GitHub: {e}"))?;
    from_github_archive(&bytes, path)
}

/// The secret a refresh is signed with, if the coordinator takes one.
pub fn refresh_secret() -> Option<String> {
    std::env::var(REFRESH_SECRET)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether a refresh request is signed with `secret` as Hookaido signs a
/// delivery: `hex(HMAC-SHA256(secret, "<METHOD>\n<PATH>\n<TS>\n<sha256(body)>"))`
/// in `signature`, `ts` (Unix s) within five minutes of `now`.
pub fn signed(
    secret: &str,
    method: &str,
    path: &str,
    ts: &str,
    signature: &str,
    body: &[u8],
    now: u64,
) -> bool {
    use hmac::{KeyInit, Mac};
    let Ok(t) = ts.trim().parse::<u64>() else {
        return false;
    };
    if t.abs_diff(now) > 300 {
        return false;
    }
    let canonical = format!("{method}\n{path}\n{t}\n{}", hex(&Sha256::digest(body)));
    let Ok(mut mac) = hmac::Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(canonical.as_bytes());
    let want = hex(&mac.finalize().into_bytes());
    let got = signature.trim().trim_start_matches("sha256=");
    crate::tls::same(&want, &got.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(path: &str) -> SkillSet {
        SkillSet {
            name: "core".into(),
            source: Source::Folder { path: path.into() },
            to: vec!["*".into()],
        }
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn a_frontmatter_gives_name_and_description_in_its_forms() {
        let f = frontmatter("---\nname: work-issue\ndescription: \"Works on: an issue\"\n---\n# x")
            .unwrap();
        assert_eq!(
            (f.name.as_str(), f.description.as_str()),
            ("work-issue", "Works on: an issue")
        );
        assert_eq!((f.group, f.tags.len()), (None, 0));
        let f = frontmatter("---\nname: a\ndescription: >\n  Two\n  lines\nlicense: MIT\n---\n")
            .unwrap();
        assert_eq!(f.description, "Two lines");
        assert!(frontmatter("# no frontmatter").is_err());
        assert!(frontmatter("---\nname: a\n---\n").is_err());
    }

    #[test]
    fn metadata_gives_group_and_tags_in_their_forms() {
        let f = frontmatter(
            "---\nname: a\ndescription: >\n  d\nmetadata:\n  group: Business\n  tags: [biz, \"html\"]\n  author: x\nlicense: MIT\n---\n",
        )
        .unwrap();
        assert_eq!(f.description, "d");
        assert_eq!(f.group.as_deref(), Some("Business"));
        assert_eq!(f.tags, ["biz", "html"]);
        let f = frontmatter(
            "---\nname: a\nmetadata:\n  tags:\n    - time\n    - 'linear'\n  group: \"Time\"\ndescription: d\n---\n",
        )
        .unwrap();
        assert_eq!(f.tags, ["time", "linear"]);
        assert_eq!(f.group.as_deref(), Some("Time"));
        assert_eq!(f.description, "d");
    }

    #[test]
    fn a_folder_source_is_packed_and_a_wrong_skill_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(SOURCES).join("core");
        write(
            &src,
            "work-issue/SKILL.md",
            "---\nname: work-issue\ndescription: Work on an issue\n---\nDo it.",
        );
        write(&src, "work-issue/references/x.md", "more");
        write(
            &src,
            "wrong/SKILL.md",
            "---\nname: other\ndescription: d\n---\n",
        );
        write(&src, "notes/README.md", "no skill");
        let m = refresh(dir.path(), &set("core"));
        assert_eq!(m.error, None);
        assert_eq!(m.skills.len(), 1);
        assert_eq!(m.skills[0].name, "work-issue");
        assert_eq!(m.skipped.len(), 2, "{:?}", m.skipped);
        // The same files, the same archive.
        let again = refresh(dir.path(), &set("core"));
        assert_eq!(again.skills[0].sha256, m.skills[0].sha256);
        let bytes = archive(dir.path(), "core", "work-issue").unwrap();
        assert_eq!(hex(&Sha256::digest(&bytes)), m.skills[0].sha256);
        let out = dir.path().join("out");
        unpack(&bytes, &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("references/x.md")).unwrap(),
            "more"
        );
    }

    #[test]
    fn a_failed_fetch_keeps_what_the_last_one_made() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(SOURCES).join("core");
        write(&src, "a/SKILL.md", "---\nname: a\ndescription: d\n---\n");
        let ok = refresh(dir.path(), &set("core"));
        std::fs::remove_dir_all(&src).unwrap();
        let failed = refresh(dir.path(), &set("core"));
        assert!(failed.error.is_some());
        assert_eq!(failed.skills, ok.skills);
        assert!(archive(dir.path(), "core", "a").is_some());
    }

    #[test]
    fn a_set_is_checked_and_given_by_id_tag_or_star() {
        let mut s = set("core");
        assert!(s.check().is_ok());
        s.source = Source::Folder {
            path: "../data".into(),
        };
        assert!(s.check().is_err());
        s.source = Source::Github {
            repo: "nuetzliches/nuts-skills".into(),
            reference: "main".into(),
            path: "skills".into(),
        };
        assert!(s.check().is_ok());
        assert_eq!(
            s.source.label(),
            "github:nuetzliches/nuts-skills@main/skills"
        );
        s.to = vec!["team".into()];
        assert!(s.given_to("x", &["team".into()]));
        assert!(!s.given_to("x", &[]));
    }

    #[test]
    fn a_github_archive_gives_the_skills_below_its_path() {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut t = tar::Builder::new(gz);
        // As GitHub sends it: a pax global header first.
        let comment = b"52 comment=abc1234abc1234abc1234abc1234abc1234abc1
";
        let mut g = tar::Header::new_ustar();
        g.set_path("pax_global_header").unwrap();
        g.set_size(comment.len() as u64);
        g.set_entry_type(tar::EntryType::XGlobalHeader);
        g.set_cksum();
        t.append(&g, &comment[..]).unwrap();
        for (p, text) in [
            ("org-repo-abc1234/README.md", "top"),
            (
                "org-repo-abc1234/skills/a/SKILL.md",
                "---\nname: a\ndescription: d\n---\n",
            ),
            ("org-repo-abc1234/skills/a/scripts/run.sh", "echo"),
            ("org-repo-abc1234/other/b/SKILL.md", "x"),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(text.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            t.append_data(&mut h, p, text.as_bytes()).unwrap();
        }
        let bytes = t.into_inner().unwrap().finish().unwrap();
        let (found, commit) = from_github_archive(&bytes, "skills").unwrap();
        assert_eq!(commit.as_deref(), Some("abc1234"));
        assert_eq!(found.keys().collect::<Vec<_>>(), ["a"]);
        assert!(found["a"].contains_key("scripts/run.sh"));
    }

    #[test]
    fn a_refresh_is_signed_as_hookaido_signs() {
        use hmac::{KeyInit, Mac};
        let body = br#"{"ref":"refs/heads/main"}"#;
        let canonical = format!(
            "POST\n/v1/skills/refresh\n1000\n{}",
            hex(&Sha256::digest(body))
        );
        let mut mac = hmac::Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
        mac.update(canonical.as_bytes());
        let sig = hex(&mac.finalize().into_bytes());
        assert!(signed(
            "s3cret",
            "POST",
            "/v1/skills/refresh",
            "1000",
            &sig,
            body,
            1100
        ));
        assert!(!signed(
            "s3cret",
            "POST",
            "/v1/skills/refresh",
            "1000",
            &sig,
            body,
            2000
        ));
        assert!(!signed(
            "other",
            "POST",
            "/v1/skills/refresh",
            "1000",
            &sig,
            body,
            1100
        ));
        assert!(!signed(
            "s3cret",
            "POST",
            "/v1/skills/refresh",
            "1000",
            &sig,
            b"{}",
            1100
        ));
    }
}
