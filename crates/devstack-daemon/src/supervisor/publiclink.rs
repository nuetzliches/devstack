//! A service's public link: a Cloudflare Quick Tunnel (`cloudflared tunnel
//! --url`, no account) to its fixed port, so that someone without the stack
//! opens it from the internet. Only ever opened by hand (the dashboard,
//! `devstack tunnel`), never by the stack file. It goes to the fixed port,
//! which the proxy or the dev server's forward holds, so it outlasts the
//! service's restarts, rebuilds and `promote --lane`; it ends when it is
//! closed, with the service's `stop`, the tree's `down` and the daemon.

use super::Supervisor;
use crate::state::{EventBody, PublicLinkState, PublicLinkView, Status, now_ms};
use devstack_proc::{Owned, Spec};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// How long `cloudflared` gets to register its tunnel.
const OPEN_TIMEOUT: Duration = Duration::from_secs(45);

/// `cloudflared`, if not on the `PATH` (a test's fake, an install elsewhere).
const PROGRAM_VARIABLE: &str = "DEVSTACK_CLOUDFLARED";

pub(crate) struct Link {
    view: PublicLinkView,
    /// `None` once it failed: nothing runs, the view says why.
    tree: Option<Owned>,
    record: String,
    /// Tells a watcher thread of an earlier link of the service to end.
    generation: u64,
}

impl Supervisor {
    pub(crate) fn public_link_view(&self, name: &str) -> Option<PublicLinkView> {
        self.public_links
            .lock()
            .unwrap()
            .get(name)
            .map(|l| l.view.clone())
    }

    /// Open a public link to `name`'s fixed port; one already open stays.
    /// `own_ports`: devstack's own (the hub's, this daemon's API), which are
    /// never tunnelled.
    pub fn open_public_link(self: &Arc<Self>, name: &str, own_ports: &[u16]) -> Result<(), String> {
        let svc = self.service(name)?;
        if crate::remote_lane_name().is_some() {
            return Err(
                "a public link is opened on the developer's machine, not yet for a lane on a node"
                    .into(),
            );
        }
        if svc.tunnel == Some(false) {
            return Err(format!(
                "the stack file keeps `{name}` from a public link (`tunnel = false`)"
            ));
        }
        let (port, status) = {
            let slots = self.slots.lock().unwrap();
            let v = &slots[name].view;
            (v.port, v.status.clone())
        };
        let Some(port) = port else {
            return Err(format!("`{name}` has no port to open a public link to"));
        };
        if own_ports.contains(&port) {
            return Err(format!(
                "port {port} is devstack's own, and is never public"
            ));
        }
        if matches!(status, Status::Stopped) {
            return Err(format!(
                "`{name}` is stopped: start it first (devstack start {name})"
            ));
        }
        let generation = {
            let links = self.public_links.lock().unwrap();
            match links.get(name) {
                Some(l) if l.tree.is_some() => return Ok(()),
                Some(l) => l.generation + 1,
                None => 0,
            }
        };
        let program = program()?;
        let scheme = svc.scheme();
        // `localhost`, not 127.0.0.1: a service without the proxy may bind
        // IPv6 loopback only (a dev server), and cloudflared tries both. The
        // Host header is the one a browser here sends, so a dev server's
        // host check (Vite's `allowedHosts`, `ng serve`'s) lets it through.
        let mut command = format!(
            "{} tunnel --no-autoupdate --url {scheme}://localhost:{port} --http-host-header localhost:{port}",
            quoted(&program)
        );
        if scheme == "https" {
            // Its certificate is a development one (`insecure_tls`).
            command.push_str(" --no-tls-verify");
        }
        let dir = self.log_dir(name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let log = dir.join("public-link.log");
        let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        let record = format!("{name}@public-link");
        let (tree, rec) = devstack_proc::spawn(
            Spec {
                name: record.clone(),
                command,
                cwd: self.repo.clone(),
                env: Default::default(),
                log: file,
            },
            &self.proc_opts,
        )
        .map_err(|e| e.to_string())?;
        {
            let mut p = self.persisted.lock().unwrap();
            p.records.retain(|r| r.name != record);
            p.records.push(rec);
            p.save(&self.state_dir);
        }
        let view = PublicLinkView {
            state: PublicLinkState::Starting,
            url: None,
            since: now_ms(),
            error: None,
        };
        self.public_links.lock().unwrap().insert(
            name.to_string(),
            Link {
                view: view.clone(),
                tree: Some(tree),
                record,
                generation,
            },
        );
        self.emit(EventBody::PublicLink {
            service: name.into(),
            link: Some(view),
        });
        let me = Arc::downgrade(self);
        let service = name.to_string();
        std::thread::Builder::new()
            .name(format!("public-link-{name}"))
            .spawn(move || watch(me, service, generation, log))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Close `name`'s public link, if it has one (a failed one is cleared).
    pub fn close_public_link(&self, name: &str) {
        let Some(mut link) = self.public_links.lock().unwrap().remove(name) else {
            return;
        };
        if let Some(mut tree) = link.tree.take() {
            tree.stop(Duration::from_secs(3));
            self.forget_record(&link.record);
        }
        self.emit(EventBody::PublicLink {
            service: name.into(),
            link: None,
        });
    }
}

/// Follow a link's `cloudflared`: its address, its registered tunnel, its
/// end. Ends with the link (closed, opened again) or the supervisor.
fn watch(sup: Weak<Supervisor>, service: String, generation: u64, log: PathBuf) {
    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let Some(sup) = sup.upgrade() else { return };
        let output = std::fs::read_to_string(&log).unwrap_or_default();
        let changed = {
            let mut links = sup.public_links.lock().unwrap();
            let Some(link) = links
                .get_mut(&service)
                .filter(|l| l.generation == generation)
            else {
                return;
            };
            let Some(tree) = link.tree.as_mut() else {
                return;
            };
            let before = link.view.clone();
            let ended = tree.try_exit_code();
            if link.view.url.is_none() {
                link.view.url = quick_tunnel_url(&output);
            }
            if link.view.state == PublicLinkState::Starting
                && link.view.url.is_some()
                && output.contains("Registered tunnel connection")
            {
                link.view.state = PublicLinkState::Open;
            }
            let failed = match ended {
                Some(code) => Some(last_error(&output).unwrap_or_else(|| {
                    format!(
                        "cloudflared ended (exit code {code}); see {}",
                        log.display()
                    )
                })),
                None if link.view.state == PublicLinkState::Starting
                    && started.elapsed() > OPEN_TIMEOUT =>
                {
                    Some(last_error(&output).unwrap_or_else(|| {
                        format!(
                            "no tunnel after {} s; see {}",
                            OPEN_TIMEOUT.as_secs(),
                            log.display()
                        )
                    }))
                }
                None => None,
            };
            if let Some(error) = failed {
                if let Some(mut tree) = link.tree.take() {
                    tree.kill();
                }
                link.view.state = PublicLinkState::Failed;
                link.view.error = Some(error);
                sup.forget_record(&link.record);
            }
            (link.view != before).then(|| link.view.clone())
        };
        if let Some(view) = changed {
            let done = view.state == PublicLinkState::Failed;
            sup.emit(EventBody::PublicLink {
                service: service.clone(),
                link: Some(view),
            });
            if done {
                return;
            }
        }
    }
}

/// The address `cloudflared` prints for a Quick Tunnel.
fn quick_tunnel_url(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|w| {
        let w = w.trim_matches(|c: char| c == '|' || c == '"');
        let host = w
            .strip_prefix("https://")?
            .strip_suffix(".trycloudflare.com")?;
        (!host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            .then(|| w.to_string())
    })
}

/// `cloudflared`'s last error line, without its timestamp and level.
fn last_error(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find_map(|l| {
            l.split_once(" ERR ")
                .map(|(_, rest)| rest.trim().to_string())
        })
        .filter(|e| !e.is_empty())
}

fn program() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(PROGRAM_VARIABLE).filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "{PROGRAM_VARIABLE} names {}, which is not a file",
                p.display()
            ))
        };
    }
    find_on_path("cloudflared").ok_or_else(|| {
        "cloudflared is not installed, or not on the PATH of devstack's daemon: \
         `winget install Cloudflare.cloudflared` (Windows), `brew install cloudflared` (macOS), \
         https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/ (Linux); \
         then `devstack restart` so the daemon sees it"
            .into()
    })
}

/// `name` in the directories of `PATH`, as the shell would find it.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

fn quoted(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BANNER: &str = "\
2026-11-03T10:00:00Z INF Requesting new quick Tunnel on trycloudflare.com...
2026-11-03T10:00:01Z INF +--------------------------------------------------------------------------------------------+
2026-11-03T10:00:01Z INF |  Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):  |
2026-11-03T10:00:01Z INF |  https://blue-river-quiet-stone.trycloudflare.com                                          |
2026-11-03T10:00:01Z INF +--------------------------------------------------------------------------------------------+
2026-11-03T10:00:02Z INF Registered tunnel connection connIndex=0 location=fra08 protocol=quic
";

    #[test]
    fn the_address_is_read_from_the_banner() {
        assert_eq!(
            quick_tunnel_url(BANNER).as_deref(),
            Some("https://blue-river-quiet-stone.trycloudflare.com")
        );
        // The request line names the domain, not a tunnel.
        assert_eq!(
            quick_tunnel_url("INF Requesting new quick Tunnel on trycloudflare.com..."),
            None
        );
        assert_eq!(
            quick_tunnel_url("https://evil.example.com/.trycloudflare.com"),
            None
        );
    }

    #[test]
    fn the_last_error_line_is_the_reason() {
        let out = "2026-11-03T10:00:00Z INF Starting\n\
                   2026-11-03T10:00:01Z ERR failed to request quick Tunnel: 429 Too Many Requests\n";
        assert_eq!(
            last_error(out).as_deref(),
            Some("failed to request quick Tunnel: 429 Too Many Requests")
        );
        assert_eq!(last_error(BANNER), None);
    }
}
