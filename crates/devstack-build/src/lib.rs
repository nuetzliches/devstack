//! Builds (ADR-0002, ADR-0003, ADR-0020): a lane's source is synced into a
//! snapshot, built there in place with the toolchain's normal output
//! directories, and the output is published by copy into an immutable artifact
//! directory named by the build id. Processes only ever run from artifacts.
//!
//! Layout under the state directory (default `<repo>/.devstack`):
//!
//! ```text
//! lanes/<lane>/src            snapshot of the lane's source
//! lanes/<lane>/manifest.json  size, mtime and content hash per file
//! artifacts/<service>/<id>/   published build output (+ .devstack-artifact.json)
//! artifacts/<service>/.tmp-<id>  a publish in progress; removed on start
//! logs/<service>/build-<id>.log
//! ```

pub mod snapshot;

use devstack_config::interpolate::{self, Placeholder, Resolve};
use devstack_config::model::Service;
use devstack_proc::{Build, BuildJob};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use snapshot::{Manifest, Snapshot, SyncStats};

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Proc(#[from] devstack_proc::ProcError),
    #[error("invalid watch pattern `{pattern}`: {message}")]
    Glob { pattern: String, message: String },
    #[error("{0}")]
    Other(String),
}

impl BuildError {
    pub(crate) fn io(what: impl Into<String>, source: std::io::Error) -> Self {
        BuildError::Io {
            what: what.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, BuildError>;

/// Paths under the state directory.
#[derive(Debug, Clone)]
pub struct Layout {
    pub state: PathBuf,
    /// Where artifacts are stored: `<state>/artifacts`, or a store shared
    /// with other lanes (ADR-0024), owned by the main working tree.
    pub artifacts_root: PathBuf,
}

impl Layout {
    pub fn new(state: impl Into<PathBuf>) -> Self {
        let state = state.into();
        Self {
            artifacts_root: state.join("artifacts"),
            state,
        }
    }

    /// A lane's layout: its own state, the main tree's artifact store. A build
    /// id is a hash of the inputs, so a lane on the same sources starts from
    /// the main tree's artifacts instead of building.
    pub fn shared(state: impl Into<PathBuf>, artifacts_root: impl Into<PathBuf>) -> Self {
        Self {
            state: state.into(),
            artifacts_root: artifacts_root.into(),
        }
    }

    /// The artifact store belongs to this layout (not shared from elsewhere).
    pub fn owns_artifacts(&self) -> bool {
        self.artifacts_root == self.state.join("artifacts")
    }
    pub fn lane(&self, lane: &str) -> PathBuf {
        self.state.join("lanes").join(lane)
    }
    pub fn artifacts(&self, service: &str) -> PathBuf {
        self.artifacts_root.join(service)
    }
    pub fn artifact(&self, service: &str, id: &str) -> PathBuf {
        self.artifacts(service).join(id)
    }
    pub fn build_log(&self, service: &str, id: &str) -> PathBuf {
        self.state
            .join("logs")
            .join(service)
            .join(format!("build-{id}.log"))
    }
}

/// Metadata stored in every artifact directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactMeta {
    pub service: String,
    pub id: String,
    pub lane: String,
    /// Unix milliseconds.
    pub created: u128,
    pub build_ms: u128,
}

pub const META_FILE: &str = ".devstack-artifact.json";

/// Files of build systems that change what a build produces for every project
/// below them, wherever the project's own files are (#13): MSBuild's
/// `Directory.Build.*`, SDK and package settings, workspace manifests and
/// lockfiles. Compared without case.
pub const BUILD_FILES: [&str; 24] = [
    "directory.build.props",
    "directory.build.targets",
    "directory.build.rsp",
    "directory.packages.props",
    "global.json",
    "nuget.config",
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "yarn.lock",
    "tsconfig.json",
    "tsconfig.base.json",
    "cargo.toml",
    "cargo.lock",
    "go.mod",
    "go.sum",
    "pyproject.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
];

/// `path` (repository-relative, `/`) is one of [`BUILD_FILES`].
pub fn is_build_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    BUILD_FILES.contains(&name.as_str())
}

/// The build id: a hash over the service's inputs (ADR-0002). Inputs are the
/// files matched by `watch` (all files when it is empty) with their content
/// hashes; the build-system files ([`BUILD_FILES`]) in the directory of a
/// watched file or above it, and the files of `prepare.inputs`; and the build
/// command, output path, working directory and `prepare` (command, directory,
/// inputs), so changing how a service is built also yields a new id. Ignored
/// files are not in the snapshot and count for nothing.
pub fn build_id(manifest: &Manifest, svc: &Service) -> Result<String> {
    let set = watch_set(&svc.watch)?;
    // Directories that hold a watched file: a build file applies to them if it
    // sits in one of them or above.
    let dirs: BTreeSet<&str> = match &set {
        None => BTreeSet::new(),
        Some(s) => manifest
            .files
            .keys()
            .filter(|p| s.is_match(p.as_str()))
            .map(|p| p.rsplit_once('/').map_or("", |(d, _)| d))
            .collect(),
    };
    let applies = |path: &str| {
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        dir.is_empty()
            || dirs
                .iter()
                .any(|d| *d == dir || d.strip_prefix(dir).is_some_and(|r| r.starts_with('/')))
    };
    let prepare = svc.prepare.as_ref();
    // What `prepare` installs from (a lockfile) shapes the build as well.
    let prepared = watch_set(prepare.map_or(&[][..], |p| p.inputs.as_slice()))?;
    let mut h = blake3::Hasher::new();
    h.update(b"devstack build id v3\0");
    let inputs = prepare.map(|p| p.inputs.join("\n"));
    for part in [
        svc.build.as_deref(),
        svc.output.as_deref(),
        svc.cwd.as_deref(),
        prepare.map(|p| p.run.as_str()),
        prepare.and_then(|p| p.cwd.as_deref()),
        inputs.as_deref(),
    ] {
        // A tag before each part: `None` and `Some("")` differ.
        match part {
            None => h.update(b"-"),
            Some(v) => h.update(b"+").update(v.as_bytes()),
        };
        h.update(b"\0");
    }
    for (path, e) in &manifest.files {
        let counts = match &set {
            None => true,
            Some(s) => {
                s.is_match(path)
                    || (is_build_file(path) && applies(path))
                    || prepared.as_ref().is_some_and(|p| p.is_match(path))
            }
        };
        if counts {
            h.update(path.as_bytes());
            h.update(b"\0");
            h.update(e.hash.as_bytes());
            h.update(b"\0");
        }
    }
    Ok(h.finalize().to_hex()[..16].to_string())
}

/// The `watch` patterns as a glob set (`None`: no patterns).
pub fn watch_set(patterns: &[String]) -> Result<Option<globset::GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        let g = globset::GlobBuilder::new(p)
            .literal_separator(true)
            .build()
            .map_err(|e| BuildError::Glob {
                pattern: p.clone(),
                message: e.to_string(),
            })?;
        b.add(g);
    }
    b.build().map(Some).map_err(|e| BuildError::Glob {
        pattern: patterns.join(", "),
        message: e.to_string(),
    })
}

/// Owns the shared build job (ADR-0020) and the artifact store.
pub struct Builder {
    pub layout: Layout,
    job: std::sync::Arc<BuildJob>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Built and published.
    Published {
        id: String,
        dir: PathBuf,
        elapsed: Duration,
    },
    /// The build command failed; nothing was published.
    Failed {
        id: String,
        exit_code: i32,
        log: PathBuf,
    },
    Cancelled {
        id: String,
    },
}

/// A build in progress.
pub struct Running {
    pub service: String,
    pub id: String,
    pub lane: String,
    pub log: PathBuf,
    build: Build,
    output: PathBuf,
    started: Instant,
}

impl Builder {
    /// Removes publishes a previous instance left half done (ADR-0016).
    pub fn new(layout: Layout, opts: &devstack_proc::Options) -> Result<Self> {
        Ok(Self::with_job(
            layout,
            std::sync::Arc::new(BuildJob::new(opts)?),
        ))
    }

    /// [`Self::new`] on a build job that already exists: one per daemon
    /// (ADR-0020), shared by the main tree's builder and every lane's, so a
    /// lane that goes does not take the build servers of the others along.
    pub fn with_job(layout: Layout, job: std::sync::Arc<BuildJob>) -> Self {
        // Only the owner of the store clears half-done publishes: in a shared
        // store a `.tmp-` may be another lane's publish in progress.
        let root = layout.artifacts_root.clone();
        let services = if layout.owns_artifacts() {
            fs::read_dir(&root).ok()
        } else {
            None
        };
        if let Some(services) = services {
            for s in services.flatten() {
                if let Ok(entries) = fs::read_dir(s.path()) {
                    for e in entries.flatten() {
                        if e.file_name().to_string_lossy().starts_with(".tmp-") {
                            let _ = fs::remove_dir_all(e.path());
                        }
                    }
                }
            }
        }
        Self { layout, job }
    }

    /// The build job this builder runs in, to share with another builder.
    pub fn job(&self) -> std::sync::Arc<BuildJob> {
        self.job.clone()
    }

    /// What the shared build job costs right now (plan §12, ADR-0020).
    pub fn usage(&self) -> devstack_proc::Usage {
        self.job.usage()
    }

    pub fn artifact(&self, service: &str, id: &str) -> Option<ArtifactMeta> {
        let meta = fs::read(self.layout.artifact(service, id).join(META_FILE)).ok()?;
        serde_json::from_slice(&meta).ok()
    }

    /// Start building `service` from the lane snapshot `snap` (already synced).
    pub fn start(
        &self,
        lane: &str,
        snap: &Snapshot,
        service: &str,
        svc: &Service,
    ) -> Result<Running> {
        let id = build_id(&snap.manifest, svc)?;
        self.start_as(lane, snap, service, svc, id)
    }

    /// [`Self::start`], publishing under `id` (a forced rebuild of the same
    /// inputs, `<inputs>-<n>`).
    pub fn start_as(
        &self,
        lane: &str,
        snap: &Snapshot,
        service: &str,
        svc: &Service,
        id: String,
    ) -> Result<Running> {
        let (Some(command), Some(output)) = (&svc.build, &svc.output) else {
            return Err(BuildError::Other(format!(
                "service `{service}` has no `build`/`output`"
            )));
        };
        let log = self.layout.build_log(service, &id);
        fs::create_dir_all(log.parent().expect("log has a parent"))
            .map_err(|e| BuildError::io("create log dir", e))?;
        let cwd = match &svc.cwd {
            Some(c) => snap.dir.join(c),
            None => snap.dir.clone(),
        };
        // In a build, `{repo}` is the snapshot: that is where the build runs.
        let command = expand(command, &snap.dir)?;
        let output = snap.dir.join(expand(output, &snap.dir)?);
        let spec = devstack_proc::Spec {
            name: format!("build-{service}"),
            command,
            cwd,
            env: BTreeMap::new(),
            log: fs::File::create(&log)
                .map_err(|e| BuildError::io(format!("create {}", log.display()), e))?,
        };
        let build = self.job.run(spec)?;
        Ok(Running {
            service: service.into(),
            id,
            lane: lane.into(),
            log,
            build,
            output,
            started: Instant::now(),
        })
    }

    /// Non-blocking: the outcome once the build's root has exited (ADR-0020:
    /// that, not an empty job, is the end of a build), published if it succeeded.
    pub fn poll(&self, r: &mut Running) -> Result<Option<Outcome>> {
        match r.build.try_exit_code() {
            None => Ok(None),
            Some(code) => self.finish(r, code).map(Some),
        }
    }

    pub fn wait(&self, r: &mut Running) -> Result<Outcome> {
        let code = r.build.wait()?;
        self.finish(r, code)
    }

    /// Superseded (ADR-0003): kill the build's tree, publish nothing.
    pub fn cancel(&self, r: &mut Running) -> Outcome {
        r.build.cancel();
        Outcome::Cancelled { id: r.id.clone() }
    }

    fn finish(&self, r: &Running, code: i32) -> Result<Outcome> {
        if code != 0 {
            return Ok(Outcome::Failed {
                id: r.id.clone(),
                exit_code: code,
                log: r.log.clone(),
            });
        }
        let dir = self.publish(r)?;
        Ok(Outcome::Published {
            id: r.id.clone(),
            dir,
            elapsed: r.started.elapsed(),
        })
    }

    /// Copy the output into `.tmp-<id>`, write the metadata, rename into place.
    /// An artifact directory is therefore complete or absent (ADR-0002).
    fn publish(&self, r: &Running) -> Result<PathBuf> {
        if !r.output.is_dir() {
            return Err(BuildError::Other(format!(
                "build of `{}` succeeded but its output `{}` does not exist",
                r.service,
                r.output.display()
            )));
        }
        let fin = self.layout.artifact(&r.service, &r.id);
        if self.artifact(&r.service, &r.id).is_some() {
            return Ok(fin); // same inputs, already published
        }
        // A directory without metadata is no artifact (a prune that stopped
        // halfway): it goes, and the build is published afresh.
        if fin.exists() {
            fs::remove_dir_all(&fin)
                .map_err(|e| BuildError::io(format!("remove incomplete {}", fin.display()), e))?;
        }
        let tmp = self
            .layout
            .artifacts(&r.service)
            .join(format!(".tmp-{}", r.id));
        let _ = fs::remove_dir_all(&tmp);
        copy_dir(&r.output, &tmp)?;
        let meta = ArtifactMeta {
            service: r.service.clone(),
            id: r.id.clone(),
            lane: r.lane.clone(),
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
            build_ms: r.started.elapsed().as_millis(),
        };
        fs::write(
            tmp.join(META_FILE),
            serde_json::to_vec_pretty(&meta).expect("meta serialises"),
        )
        .map_err(|e| BuildError::io("write artifact meta", e))?;
        match fs::rename(&tmp, &fin) {
            Ok(()) => Ok(fin),
            Err(_) if fin.exists() => {
                let _ = fs::remove_dir_all(&tmp); // published concurrently with the same id
                Ok(fin)
            }
            Err(e) => Err(BuildError::io(format!("publish {}", fin.display()), e)),
        }
    }

    /// Keep the newest `keep` artifacts of a service plus every pinned id
    /// (main slot, held candidates); remove the rest. Returns removed ids.
    pub fn prune(&self, service: &str, keep: usize, pinned: &[&str]) -> Vec<String> {
        let mut all: Vec<ArtifactMeta> = fs::read_dir(self.layout.artifacts(service))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| self.artifact(service, &e.file_name().to_string_lossy()))
            .collect();
        all.sort_by_key(|a| std::cmp::Reverse(a.created));
        let dir = self.layout.artifacts(service);
        let mut removed = Vec::new();
        for (i, a) in all.iter().enumerate() {
            if i < keep || pinned.contains(&a.id.as_str()) {
                continue;
            }
            // Renamed away first, so the id is gone at once or not at all: a
            // delete that stops halfway (a file still open on Windows) leaves
            // a `.trash-` directory, never a half artifact under its id. A
            // directory with a file in use cannot be renamed on Windows; it
            // stays for the next prune.
            let trash = dir.join(format!(".trash-{}", a.id));
            let _ = fs::remove_dir_all(&trash);
            if fs::rename(self.layout.artifact(service, &a.id), &trash).is_ok() {
                let _ = fs::remove_dir_all(&trash);
                removed.push(a.id.clone());
            }
        }
        // What an earlier prune could not finish.
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(".trash-") {
                let _ = fs::remove_dir_all(e.path());
            }
        }
        removed
    }
}

/// `build` or `output` as the stack file's interpolation reads them
/// (validation allows `{repo}` and `${VAR}` there): `{repo}` is the snapshot,
/// `${VAR}` comes from the daemon's environment, `{{`/`}}` are braces. The
/// same on every platform, before the shell sees the line.
fn expand(s: &str, snapshot: &Path) -> Result<String> {
    struct Snap<'a>(&'a Path);
    impl Resolve for Snap<'_> {
        fn resolve(&self, p: &Placeholder) -> std::result::Result<String, String> {
            match p {
                Placeholder::Repo => Ok(self.0.to_string_lossy().into_owned()),
                other => Err(format!("`{other}` is not known to a build")),
            }
        }
    }
    interpolate::expand(s, &Snap(snapshot), &|k| std::env::var(k).ok())
        .map_err(|e| BuildError::Other(format!("`{s}`: {e}")))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(|e| BuildError::io(format!("create {}", to.display()), e))?;
    for e in
        fs::read_dir(from).map_err(|e| BuildError::io(format!("read {}", from.display()), e))?
    {
        let e = e.map_err(|e| BuildError::io("read dir entry", e))?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type()
            .map_err(|e| BuildError::io("file type", e))?
            .is_dir()
        {
            copy_dir(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)
                .map_err(|e| BuildError::io(format!("copy {}", src.display()), e))?;
        }
    }
    Ok(())
}
