//! A service's public link: a Cloudflare Quick Tunnel (`cloudflared tunnel
//! --url`, no account) to its fixed port, so that someone without the stack
//! opens it from the internet. Only ever opened by hand (the dashboard,
//! `devstack tunnel`), never by the stack file. It goes to the fixed port,
//! which the proxy or the dev server's forward holds, so it outlasts the
//! service's restarts, rebuilds and `promote --lane`; it ends when it is
//! closed, with the service's `stop`, the tree's `down` and the daemon.
//!
//! With the user's named tunnel set up (ADR-0090, step 2: `devstack tunnel
//! --setup`), no Quick Tunnel runs: the link's address is the stable
//! `https://{service}-{lane}-{stack}.{domain}`, and the hub, which follows
//! this tree's events, routes it through its one connector; the link is
//! open once the hub says that connector runs and has the host.

use super::Supervisor;
use crate::state::{EventBody, PublicLinkState, PublicLinkView, Status, now_ms};
use devstack_proc::{Owned, Spec};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// How long `cloudflared` gets to register its tunnel.
const OPEN_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) struct Link {
    view: PublicLinkView,
    /// The Quick Tunnel's `cloudflared`; `None` once it failed, and for a
    /// link through the named tunnel (the hub runs that one).
    tree: Option<Owned>,
    record: String,
    /// Tells a watcher thread of an earlier link of the service to end.
    generation: u64,
}

impl Supervisor {
    /// `{service.<name>.public_url}` of this tree (ADR-0090).
    pub(crate) fn public_urls(&self) -> std::collections::BTreeMap<String, String> {
        crate::namedtunnel::public_urls(&self.stack, &self.lane)
    }

    /// Open a public link to every service of this tree that may have one
    /// and runs, those already open kept ("make the lane public"). The
    /// services it opened, and why the others were not.
    pub fn open_public_links(self: &Arc<Self>, own_ports: &[u16]) -> (Vec<String>, Vec<String>) {
        let names: Vec<String> = {
            let slots = self.slots.lock().unwrap();
            slots
                .values()
                .filter(|s| {
                    s.view.public_link_available && !matches!(s.view.status, Status::Stopped)
                })
                .map(|s| s.view.name.clone())
                .collect()
        };
        let (mut opened, mut failed) = (Vec::new(), Vec::new());
        for n in names {
            match self.open_public_link(&n, own_ports) {
                Ok(()) => opened.push(n),
                Err(e) => failed.push(format!("{n}: {e}")),
            }
        }
        (opened, failed)
    }

    /// Close every public link of this tree.
    pub fn close_public_links(&self) {
        let names: Vec<String> = self.public_links.lock().unwrap().keys().cloned().collect();
        for n in names {
            self.close_public_link(&n);
        }
    }

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
                Some(l) if l.view.state != PublicLinkState::Failed => return Ok(()),
                Some(l) => l.generation + 1,
                None => 0,
            }
        };
        if let Some(tunnel) = crate::namedtunnel::read() {
            let host = tunnel.host(&self.stack.stack.name, &self.lane, name);
            let view = PublicLinkView {
                state: PublicLinkState::Starting,
                url: Some(format!("https://{host}")),
                since: now_ms(),
                error: None,
            };
            self.public_links.lock().unwrap().insert(
                name.to_string(),
                Link {
                    view: view.clone(),
                    tree: None,
                    record: String::new(),
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
                .spawn(move || watch_named(me, service, generation, host))
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        let program = crate::namedtunnel::cloudflared()?;
        let scheme = svc.scheme();
        // `localhost`, not 127.0.0.1: a service without the proxy may bind
        // IPv6 loopback only (a dev server), and cloudflared tries both. The
        // Host header is the one a browser here sends, so a dev server's
        // host check (Vite's `allowedHosts`, `ng serve`'s) lets it through.
        let mut command = format!(
            "{} tunnel --no-autoupdate --url {scheme}://localhost:{port} --http-host-header localhost:{port}",
            crate::namedtunnel::quoted(&program)
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
                Some(code) => Some(
                    crate::namedtunnel::cloudflared_error(&output).unwrap_or_else(|| {
                        format!(
                            "cloudflared ended (exit code {code}); see {}",
                            log.display()
                        )
                    }),
                ),
                None if link.view.state == PublicLinkState::Starting
                    && started.elapsed() > OPEN_TIMEOUT =>
                {
                    Some(
                        crate::namedtunnel::cloudflared_error(&output).unwrap_or_else(|| {
                            format!(
                                "no tunnel after {} s; see {}",
                                OPEN_TIMEOUT.as_secs(),
                                log.display()
                            )
                        }),
                    )
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

/// Wait for the hub's connector to carry a link through the named tunnel:
/// `open` once it runs and routes `host`, `failed` when it failed, the hub
/// is not there, or neither came within [`OPEN_TIMEOUT`].
fn watch_named(sup: Weak<Supervisor>, service: String, generation: u64, host: String) {
    let started = Instant::now();
    let mut unreachable_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let outcome = match crate::namedtunnel::hub_links() {
            // The pool refused the address (ADR-0092): said as it said it.
            Ok(v) if v["refused"][host.as_str()].is_string() => Some(Err(format!(
                "the pool gives {host} no record: {}",
                v["refused"][host.as_str()].as_str().unwrap_or_default()
            ))),
            Ok(v) => {
                unreachable_since = None;
                let routed = v["hosts"]
                    .as_array()
                    .is_some_and(|h| h.iter().any(|x| x == host.as_str()));
                match v["connector"]["state"].as_str() {
                    Some("running") if routed => Some(Ok(())),
                    Some("failed") => Some(Err(v["connector"]["error"]
                        .as_str()
                        .unwrap_or("the hub's cloudflared failed")
                        .to_string())),
                    _ => None,
                }
            }
            Err(e) => {
                let since = *unreachable_since.get_or_insert_with(Instant::now);
                (since.elapsed() > Duration::from_secs(5)).then(|| {
                    Err(format!(
                        "the hub runs the named tunnel, and does not answer ({e}): `devstack ui` starts it"
                    ))
                })
            }
        };
        let outcome = outcome.or_else(|| {
            (started.elapsed() > OPEN_TIMEOUT).then(|| {
                Err(format!(
                    "the hub's connector did not carry {host} within {} s (`devstack tunnel` shows its state)",
                    OPEN_TIMEOUT.as_secs()
                ))
            })
        });
        let Some(outcome) = outcome else { continue };
        let Some(sup) = sup.upgrade() else { return };
        let view = {
            let mut links = sup.public_links.lock().unwrap();
            let Some(link) = links
                .get_mut(&service)
                .filter(|l| l.generation == generation)
            else {
                return;
            };
            match outcome {
                Ok(()) => link.view.state = PublicLinkState::Open,
                Err(e) => {
                    link.view.state = PublicLinkState::Failed;
                    link.view.error = Some(e);
                }
            }
            link.view.clone()
        };
        sup.emit(EventBody::PublicLink {
            service,
            link: Some(view),
        });
        return;
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
}
