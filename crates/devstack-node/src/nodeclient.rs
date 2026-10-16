//! A requester's client for one node (ADR-0029): admit the lease the
//! coordinator offered, renew and release it, and sync a lane's files to the
//! node's mirror by content: hash every file, ask which blobs the node lacks,
//! upload only those, then have it make the mirror exactly that list.

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

    /// Start the lane from its mirror (infrastructure, lane container,
    /// daemon); `stack_file` relative to the lane's root.
    ///
    /// The node starts it in the background (a first start pulls or builds
    /// images); this waits until the lane is ready or its start failed, at
    /// most `within`, and tells `phase` every new step.
    pub async fn start(
        &self,
        lease: &str,
        stack_file: &str,
        display_root: Option<&str>,
        port_offset: u16,
        within: std::time::Duration,
        phase: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<LaneState> {
        let _: LaneState = self
            .json(
                "POST",
                &format!("/v1/leases/{lease}/start"),
                Some(&Start {
                    stack_file: stack_file.into(),
                    display_root: display_root.map(str::to_string),
                    port_offset,
                }),
            )
            .await?;
        let until = std::time::Instant::now() + within;
        let mut last = String::new();
        let mut renewed = std::time::Instant::now();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            // The lease lives two minutes unless renewed, a start may take more.
            if renewed.elapsed() >= std::time::Duration::from_secs(30) {
                self.renew(lease).await?;
                renewed = std::time::Instant::now();
            }
            let st = self.lane(lease).await?;
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
        let root = root.to_path_buf();
        let list = files.to_vec();
        let elsewhere = elsewhere.clone();
        let entries: Vec<(FileEntry, std::path::PathBuf, u64)> =
            tokio::task::spawn_blocking(move || hash_all(&root, &list, &elsewhere))
                .await
                .map_err(|e| ClientError::Other(e.to_string()))??;
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
            let p = path.clone();
            let data = tokio::task::spawn_blocking(move || std::fs::read(&p))
                .await
                .map_err(|e| ClientError::Other(e.to_string()))?
                .map_err(|e| ClientError::Io(path.display().to_string(), e))?;
            // The file may have changed since it was hashed: send what the
            // hash says, or skip it until the next sync.
            if blake3::hash(&data).to_hex().as_str() != h {
                continue;
            }
            self.call(
                "PUT",
                &format!("/v1/blobs/{h}"),
                Some(("application/octet-stream", bytes::Bytes::from(data))),
            )
            .await?;
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
}

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
/// simply not there).
fn hash_all(
    root: &Path,
    files: &[String],
    elsewhere: &Elsewhere,
) -> Result<Vec<(FileEntry, std::path::PathBuf, u64)>> {
    let mut out = Vec::with_capacity(files.len());
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
        let data =
            std::fs::read(&path).map_err(|e| ClientError::Io(path.display().to_string(), e))?;
        out.push((
            FileEntry {
                path: rel.replace('\\', "/"),
                hash: blake3::hash(&data).to_hex().to_string(),
                exec: executable(&meta),
            },
            path,
            meta.len(),
        ));
    }
    Ok(out)
}
