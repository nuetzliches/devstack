//! A requester's client for one node (ADR-0029): admit the lease the
//! coordinator offered, renew and release it, and sync a lane's files to the
//! node's mirror by content: hash every file (or take the hash an earlier sync
//! found, [`HashCache`]), ask which blobs the node lacks, upload only those (in
//! chunks), then have it make the mirror exactly that list.

use crate::coordinator::{LeaseOffer, NodeRef};
use crate::member::Local;
use crate::node::{
    Admit, Applied, Apply, FileEntry, Hashes, LaneState, NodeLease, NodeView, Start,
};
use crate::tls::{self, Answer};
use serde::de::DeserializeOwned;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{0}")]
    Tls(#[from] tls::TlsError),
    #[error("{0}")]
    Member(#[from] crate::member::MemberError),
    /// The node answered, with an error.
    #[error("{0}: {1}")]
    Status(u16, String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// What one sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub files: usize,
    /// Blobs the node lacked, and their bytes.
    pub uploaded: usize,
    pub bytes: u64,
    /// What the node changed in the mirror.
    pub written: usize,
    pub removed: usize,
}

#[derive(Clone)]
pub struct NodeClient {
    config: Arc<rustls::ClientConfig>,
    pub node: NodeRef,
}

impl NodeClient {
    /// A client for `node`, as the member in `local`.
    pub fn new(local: &Local, node: NodeRef) -> Result<Self> {
        let (ca, cert, key) = local.pems()?;
        Ok(Self {
            config: Arc::new(tls::client_config(&ca, &cert, &key)?),
            node,
        })
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<(&'static str, bytes::Bytes)>,
    ) -> Result<Answer> {
        let a = tls::request(
            self.config.clone(),
            &self.node.api,
            &self.node.id,
            method,
            path,
            body,
        )
        .await?;
        if (200..300).contains(&a.status) {
            Ok(a)
        } else {
            Err(ClientError::Status(
                a.status,
                String::from_utf8_lossy(&a.body).trim().to_string(),
            ))
        }
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&impl serde::Serialize>,
    ) -> Result<T> {
        let body = body.map(|b| {
            (
                "application/json",
                bytes::Bytes::from(serde_json::to_vec(b).expect("a request serialises")),
            )
        });
        let a = self.call(method, path, body).await?;
        serde_json::from_slice(&a.body)
            .map_err(|e| ClientError::Other(format!("{path}: unexpected answer: {e}")))
    }

    pub async fn node(&self) -> Result<NodeView> {
        self.json("GET", "/v1/node", None::<&()>).await
    }

    /// Bring the coordinator's offer to the node, which admits it or not.
    pub async fn admit(&self, offer: &LeaseOffer) -> Result<NodeLease> {
        self.json(
            "POST",
            "/v1/leases",
            Some(&Admit {
                id: offer.id.clone(),
                token: offer.token.clone(),
            }),
        )
        .await
    }

    pub async fn renew(&self, lease: &str) -> Result<NodeLease> {
        self.json("POST", &format!("/v1/leases/{lease}/renew"), None::<&()>)
            .await
    }

    pub async fn release(&self, lease: &str) -> Result<()> {
        self.call("DELETE", &format!("/v1/leases/{lease}"), None)
            .await
            .map(|_| ())
    }

    /// This member's lanes the node keeps (ADR-0038).
    pub async fn slots(&self) -> Result<Vec<crate::node::SlotView>> {
        self.json("GET", "/v1/slots", None::<&()>).await
    }

    /// Throw this member's kept lane `lane` of `repo` away on the node.
    pub async fn purge_slot(&self, repo: &str, lane: &str) -> Result<()> {
        let q = |v: &str| {
            v.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        };
        self.call(
            "DELETE",
            &format!("/v1/slots?repo={}&lane={}", q(repo), q(lane)),
            None,
        )
        .await
        .map(|_| ())
    }

    /// Start the lane from its mirror (infrastructure, lane container,
    /// daemon); `stack_file` relative to the lane's root.
    ///
    /// The node starts it in the background (a first start pulls or builds
    /// images); this waits until the lane is ready or its start failed, at
    /// most `within`, and tells `phase` every new step and `over` every step
    /// that is over, with how long it took as the node measured it (an
    /// older agent tells no times).
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        &self,
        lease: &str,
        stack_file: &str,
        display_root: Option<&str>,
        port_offset: u16,
        profile: Option<&str>,
        within: std::time::Duration,
        phase: &(dyn Fn(&str) + Send + Sync),
        over: &(dyn Fn(&str, u64) + Send + Sync),
    ) -> Result<LaneState> {
        let _: LaneState = self
            .json(
                "POST",
                &format!("/v1/leases/{lease}/start"),
                Some(&Start {
                    stack_file: stack_file.into(),
                    display_root: display_root.map(str::to_string),
                    port_offset,
                    profile: profile.map(str::to_string),
                }),
            )
            .await?;
        let until = std::time::Instant::now() + within;
        let mut last = String::new();
        let mut renewed = std::time::Instant::now();
        let mut told = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            // The lease lives two minutes unless renewed, a start may take more.
            if renewed.elapsed() >= std::time::Duration::from_secs(30) {
                self.renew(lease).await?;
                renewed = std::time::Instant::now();
            }
            let st = self.lane(lease).await?;
            for t in st.phases.iter().skip(told) {
                over(&t.phase, t.ms);
            }
            told = told.max(st.phases.len());
            if let Some(e) = st.error {
                return Err(ClientError::Other(format!("starting the lane: {e}")));
            }
            if st.ready {
                return Ok(st);
            }
            if let Some(p) = &st.phase
                && *p != last
            {
                phase(p);
                last = p.clone();
            }
            if std::time::Instant::now() > until {
                return Err(ClientError::Other(format!(
                    "the lane did not start within {} min (last: {last})",
                    within.as_secs() / 60
                )));
            }
        }
    }

    pub async fn stop(&self, lease: &str) -> Result<LaneState> {
        self.json("POST", &format!("/v1/leases/{lease}/stop"), None::<&()>)
            .await
    }

    pub async fn lane(&self, lease: &str) -> Result<LaneState> {
        self.json("GET", &format!("/v1/leases/{lease}/lane"), None::<&()>)
            .await
    }

    /// A call to the lane daemon's API (`path` as the daemon has it,
    /// `/v1/stack`), answered whole.
    pub async fn api(
        &self,
        lease: &str,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, String)> {
        let body = body.map(|b| {
            (
                "application/json",
                bytes::Bytes::from(serde_json::to_vec(&b).expect("a request serialises")),
            )
        });
        let a = tls::request(
            self.config.clone(),
            &self.node.api,
            &self.node.id,
            method,
            &format!("/v1/leases/{lease}/api{path}"),
            body,
        )
        .await?;
        Ok((a.status, String::from_utf8_lossy(&a.body).into_owned()))
    }

    /// A request to the lane daemon's API, answered as it arrives (event
    /// streams included); `req`'s URI is the daemon's path, `/v1/events`.
    pub async fn forward<B>(
        &self,
        lease: &str,
        mut req: hyper::Request<B>,
    ) -> Result<hyper::Response<hyper::body::Incoming>>
    where
        B: hyper::body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let path = req
            .uri()
            .path_and_query()
            .map_or("/", |p| p.as_str())
            .to_string();
        *req.uri_mut() = format!("/v1/leases/{lease}/api{path}")
            .parse()
            .map_err(|e| ClientError::Other(format!("{path}: {e}")))?;
        Ok(tls::send(self.config.clone(), &self.node.api, &self.node.id, req).await?)
    }

    /// A read of a lane someone lets this machine view (ADR-0040), answered
    /// as it arrives; `req`'s URI is the daemon's path (`/v1/stack`). The
    /// node checks that it may.
    pub async fn view<B>(
        &self,
        lease: &str,
        mut req: hyper::Request<B>,
    ) -> Result<hyper::Response<hyper::body::Incoming>>
    where
        B: hyper::body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let path = req
            .uri()
            .path_and_query()
            .map_or("/", |p| p.as_str())
            .to_string();
        *req.uri_mut() = format!("/v1/leases/{lease}/view{path}")
            .parse()
            .map_err(|e| ClientError::Other(format!("{path}: {e}")))?;
        Ok(tls::send(self.config.clone(), &self.node.api, &self.node.id, req).await?)
    }

    /// A connection to `port` of a lane someone shares with this machine
    /// (ADR-0035); the node checks the sharing.
    pub async fn open_shared(&self, lease: &str, port: u16) -> Result<hyper::upgrade::Upgraded> {
        let req = hyper::Request::builder()
            .method("GET")
            .uri(format!("/v1/leases/{lease}/shared/{port}"))
            .header(hyper::header::CONNECTION, "upgrade")
            .header(hyper::header::UPGRADE, "devstack-tcp")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .map_err(|e| ClientError::Other(e.to_string()))?;
        let mut res = tls::send(self.config.clone(), &self.node.api, &self.node.id, req).await?;
        if res.status() != hyper::StatusCode::SWITCHING_PROTOCOLS {
            use http_body_util::BodyExt;
            let code = res.status().as_u16();
            let text = res
                .into_body()
                .collect()
                .await
                .map(|b| String::from_utf8_lossy(&b.to_bytes()).into_owned())
                .unwrap_or_default();
            return Err(ClientError::Status(code, text));
        }
        hyper::upgrade::on(&mut res)
            .await
            .map_err(|e| ClientError::Other(e.to_string()))
    }

    /// Make the node's mirror of `lease` hold exactly `files` (paths relative
    /// to `root`, `/`-separated), uploading only the content it lacks.
    pub async fn sync(&self, lease: &str, root: &Path, files: &[String]) -> Result<SyncStats> {
        self.sync_with(lease, root, files, &|_, _, _| {}).await
    }

    /// [`NodeClient::sync`], telling `uploaded` after each blob: how many of
    /// how many the node lacked, and the bytes so far.
    pub async fn sync_with(
        &self,
        lease: &str,
        root: &Path,
        files: &[String],
        uploaded: &(dyn Fn(usize, usize, u64) + Send + Sync),
    ) -> Result<SyncStats> {
        self.sync_from(lease, root, files, &Elsewhere::new(), uploaded)
            .await
    }

    /// [`NodeClient::sync_with`], with some of the files read from
    /// elsewhere than `root` (`[lanes] sync`: the main tree's copy of an
    /// ignored file the worktree lacks).
    pub async fn sync_from(
        &self,
        lease: &str,
        root: &Path,
        files: &[String],
        elsewhere: &Elsewhere,
        uploaded: &(dyn Fn(usize, usize, u64) + Send + Sync),
    ) -> Result<SyncStats> {
        self.sync_cached(
            lease,
            root,
            files,
            elsewhere,
            &mut HashCache::default(),
            uploaded,
        )
        .await
    }

    /// [`NodeClient::sync_from`], with the hashes of earlier syncs: a file
    /// whose size and modification time did not change is not read again.
    /// `cache` is left with what this sync hashed.
    pub async fn sync_cached(
        &self,
        lease: &str,
        root: &Path,
        files: &[String],
        elsewhere: &Elsewhere,
        cache: &mut HashCache,
        uploaded: &(dyn Fn(usize, usize, u64) + Send + Sync),
    ) -> Result<SyncStats> {
        let root = root.to_path_buf();
        let list = files.to_vec();
        let elsewhere = elsewhere.clone();
        let mut known = std::mem::take(cache);
        let (hashed, known) = tokio::task::spawn_blocking(move || {
            let hashed = hash_all(&root, &list, &elsewhere, &mut known);
            (hashed, known)
        })
        .await
        .map_err(|e| ClientError::Other(e.to_string()))?;
        *cache = known;
        let entries: Vec<(FileEntry, std::path::PathBuf, u64)> = hashed?;
        let missing: Hashes = self
            .json(
                "POST",
                &format!("/v1/leases/{lease}/sync/missing"),
                Some(&Hashes {
                    hashes: entries.iter().map(|(e, _, _)| e.hash.clone()).collect(),
                }),
            )
            .await?;
        let mut stats = SyncStats {
            files: entries.len(),
            ..SyncStats::default()
        };
        for h in &missing.hashes {
            let Some((_, path, size)) = entries.iter().find(|(e, _, _)| &e.hash == h) else {
                continue;
            };
            if !self.upload(h, path).await? {
                // The file changed since it was hashed: the node refused
                // it; the next sync sends what it holds then.
                cache.0.remove(path);
                continue;
            }
            stats.uploaded += 1;
            stats.bytes += size;
            uploaded(stats.uploaded, missing.hashes.len(), stats.bytes);
        }
        let applied: Applied = self
            .json(
                "POST",
                &format!("/v1/leases/{lease}/sync/apply"),
                Some(&Apply {
                    files: entries.into_iter().map(|(e, _, _)| e).collect(),
                }),
            )
            .await?;
        stats.written = applied.written;
        stats.removed = applied.removed;
        Ok(stats)
    }

    /// Send the file at `path` as blob `hash`, read and sent in chunks (a
    /// blob may be hundreds of MiB). `false` when it no longer has that hash
    /// (it changed since it was hashed).
    async fn upload(&self, hash: &str, path: &Path) -> Result<bool> {
        use http_body_util::BodyExt;
        let (tx, body) = http_body_util::channel::Channel::<bytes::Bytes, std::io::Error>::new(4);
        let req = hyper::Request::builder()
            .method("PUT")
            .uri(format!("/v1/blobs/{hash}"))
            .header(hyper::header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .map_err(|e| ClientError::Other(e.to_string()))?;
        let reader = tokio::spawn(read_into(path.to_path_buf(), tx));
        let answer = tokio::time::timeout(UPLOAD_WITHIN, async {
            let res = tls::send(self.config.clone(), &self.node.api, &self.node.id, req).await?;
            let status = res.status().as_u16();
            let text = res
                .into_body()
                .collect()
                .await
                .map(|b| String::from_utf8_lossy(&b.to_bytes()).trim().to_string())
                .unwrap_or_default();
            Ok::<_, ClientError>((status, text))
        })
        .await;
        let read = reader
            .await
            .map_err(|e| ClientError::Other(e.to_string()))?;
        let (status, text) = answer.map_err(|_| {
            ClientError::Other(format!(
                "{}: not sent within {} min",
                path.display(),
                UPLOAD_WITHIN.as_secs() / 60
            ))
        })??;
        let sent = read.map_err(|e| ClientError::Io(path.display().to_string(), e))?;
        if (200..300).contains(&status) {
            Ok(true)
        } else if sent != hash && status == 400 && text.contains("does not have that hash") {
            Ok(false)
        } else {
            Err(ClientError::Status(status, text))
        }
    }
}

/// How long one blob may take to upload.
const UPLOAD_WITHIN: std::time::Duration = std::time::Duration::from_secs(1800);

/// Read `path` into an upload body chunk by chunk; the hash of what was sent.
async fn read_into(
    path: std::path::PathBuf,
    mut tx: http_body_util::channel::Sender<bytes::Bytes, std::io::Error>,
) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            tx.abort(std::io::Error::new(e.kind(), e.to_string()));
            return Err(e);
        }
    };
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 << 10];
    loop {
        let n = match file.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                // Not a short body that looks complete: the request fails.
                tx.abort(std::io::Error::new(e.kind(), e.to_string()));
                return Err(e);
            }
        };
        hasher.update(&buf[..n]);
        if tx
            .send_data(bytes::Bytes::copy_from_slice(&buf[..n]))
            .await
            .is_err()
        {
            // The node answered early (refused it); its answer says why.
            break;
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// What earlier syncs hashed, by the path they read: the size and
/// modification time the file had then, and its hash. A caller that syncs
/// the same tree again (a remote lane) keeps it, so unchanged files are not
/// read again (A-4).
#[derive(Debug, Default)]
pub struct HashCache(std::collections::HashMap<std::path::PathBuf, Known>);

#[derive(Debug, Clone)]
struct Known {
    len: u64,
    modified: std::time::SystemTime,
    hash: String,
}

impl HashCache {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A file modified this recently may change again within the resolution of
/// its modification time, unseen: hashed every time until it is older.
const SETTLED: std::time::Duration = std::time::Duration::from_secs(2);

/// Files of a sync that are read from another path than the root's: relative
/// path in the lane, where to read it.
pub type Elsewhere = std::collections::HashMap<String, std::path::PathBuf>;

fn executable(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Hash every listed file that exists (a file deleted since it was listed is
/// simply not there), taking the hash from `cache` when the file's size and
/// modification time are what they were. `cache` is left with exactly the
/// files of this list.
fn hash_all(
    root: &Path,
    files: &[String],
    elsewhere: &Elsewhere,
    cache: &mut HashCache,
) -> Result<Vec<(FileEntry, std::path::PathBuf, u64)>> {
    let mut out = Vec::with_capacity(files.len());
    let mut next = std::collections::HashMap::with_capacity(files.len());
    let now = std::time::SystemTime::now();
    for rel in files {
        let path = elsewhere
            .get(rel)
            .cloned()
            .unwrap_or_else(|| root.join(rel));
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().ok();
        let cached = cache
            .0
            .remove(&path)
            .filter(|k| k.len == meta.len() && Some(k.modified) == modified);
        let hash = match cached {
            Some(k) => k.hash,
            None => hash_file(&path).map_err(|e| ClientError::Io(path.display().to_string(), e))?,
        };
        if let Some(modified) = modified
            && now.duration_since(modified).is_ok_and(|age| age >= SETTLED)
        {
            next.insert(
                path.clone(),
                Known {
                    len: meta.len(),
                    modified,
                    hash: hash.clone(),
                },
            );
        }
        out.push((
            FileEntry {
                path: rel.replace('\\', "/"),
                hash,
                exec: executable(&meta),
            },
            path,
            meta.len(),
        ));
    }
    cache.0 = next;
    Ok(out)
}

/// The BLAKE3 hash of a file, read in chunks.
fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_files_are_not_hashed_again() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.txt");
        std::fs::write(&f, "one").unwrap();
        // Old enough to be trusted by its size and time.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let list = vec!["a.txt".to_string()];
        let mut cache = HashCache::default();
        let first = hash_all(d.path(), &list, &Elsewhere::new(), &mut cache).unwrap();
        assert_eq!(first[0].0.hash, blake3::hash(b"one").to_hex().to_string());
        assert_eq!(cache.len(), 1);
        // A lie in the cache shows it is used as long as size and time hold.
        cache.0.get_mut(&f).unwrap().hash = "cached".into();
        let again = hash_all(d.path(), &list, &Elsewhere::new(), &mut cache).unwrap();
        assert_eq!(again[0].0.hash, "cached");
        // A change (same size, new time) is read again.
        std::fs::write(&f, "two").unwrap();
        let changed = hash_all(d.path(), &list, &Elsewhere::new(), &mut cache).unwrap();
        assert_eq!(changed[0].0.hash, blake3::hash(b"two").to_hex().to_string());
        // Just written: not trusted by its time yet.
        assert!(cache.is_empty());
        // A file no longer listed leaves the cache.
        let none = hash_all(d.path(), &[], &Elsewhere::new(), &mut cache).unwrap();
        assert!(none.is_empty() && cache.is_empty());
    }
}
