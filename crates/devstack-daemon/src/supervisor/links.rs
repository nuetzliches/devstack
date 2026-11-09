//! A service's own link, read from its output (`link = { from_log }`,
//! ADR-0101): a URL with a token or a path the instance prints at start. Each
//! instance's log is read from its beginning until the first match; the
//! link belongs to that instance and goes when another takes the main slot
//! (a candidate's line never shows before it is promoted, since only the
//! main instance's log is read).
//!
//! The link may hold a secret: it lives in `ServiceView::link`, apart from
//! `url` (which the public links and the pool read), and its event is never
//! recorded in the diagnostic log.

use super::Supervisor;
use super::rebuilds::new_lines;
use crate::state::{EventBody, Status};
use regex::Regex;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

const EVERY: Duration = Duration::from_millis(400);

/// One instance's log, as read.
struct Run {
    log: PathBuf,
    pos: u64,
    partial: String,
    /// Its link was found: the log is not read any further.
    found: bool,
}

impl Supervisor {
    /// Read the link of every service with `link = { from_log }` from its
    /// running instance's output, as long as this supervisor lives.
    pub fn follow_links(self: &Arc<Self>) {
        let me: Weak<Self> = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("links".into())
            .spawn(move || {
                let mut runs: BTreeMap<String, Run> = BTreeMap::new();
                let mut patterns: BTreeMap<String, Regex> = BTreeMap::new();
                loop {
                    std::thread::sleep(EVERY);
                    let Some(sup) = me.upgrade() else {
                        return;
                    };
                    sup.link_round(&mut runs, &mut patterns);
                }
            });
    }

    fn link_round(&self, runs: &mut BTreeMap<String, Run>, patterns: &mut BTreeMap<String, Regex>) {
        let stack = self.stack();
        runs.retain(|name, _| stack.services.get(name).is_some_and(|s| s.link.is_some()));
        for (name, svc) in &stack.services {
            let Some(link) = &svc.link else {
                continue;
            };
            // `devstack check` compiled it; a stack file changed since is
            // checked again before it runs.
            let re = match patterns.get(name) {
                Some(re) if re.as_str() == link.from_log => re.clone(),
                _ => {
                    let Ok(re) = Regex::new(&link.from_log) else {
                        continue;
                    };
                    patterns.insert(name.clone(), re.clone());
                    re
                }
            };
            let seen = self.slots.lock().unwrap().get(name).map(|s| {
                (
                    s.view.log.clone(),
                    s.view.status.clone(),
                    s.view.port,
                    s.view.instance_port,
                )
            });
            let Some((log, status, port, instance_port)) = seen else {
                continue;
            };
            let running = matches!(
                status,
                Status::Starting | Status::Running | Status::Healthy | Status::Unhealthy { .. }
            );
            let Some(log) = log.filter(|_| running) else {
                if runs.remove(name).is_some() {
                    self.set_link(name, None);
                }
                continue;
            };
            let run = match runs.get_mut(name) {
                Some(r) if r.log == log => r,
                _ => {
                    // Another instance: the last one's link is gone.
                    self.set_link(name, None);
                    runs.insert(
                        name.clone(),
                        Run {
                            log,
                            pos: 0,
                            partial: String::new(),
                            found: false,
                        },
                    );
                    runs.get_mut(name).expect("inserted")
                }
            };
            if run.found {
                continue;
            }
            let found = new_lines(&run.log, &mut run.pos, &mut run.partial)
                .iter()
                .find_map(|line| link_in(&re, line));
            if let Some(found) = found {
                run.found = true;
                self.set_link(name, Some(on_service_port(&found, instance_port, port)));
            }
        }
    }

    /// The link on the service's view and to the dashboard, if it changed;
    /// never to the diagnostic log.
    fn set_link(&self, name: &str, link: Option<String>) {
        {
            let mut slots = self.slots.lock().unwrap();
            let Some(slot) = slots.get_mut(name) else {
                return;
            };
            if slot.view.link == link {
                return;
            }
            slot.view.link = link.clone();
        }
        self.emit_quiet(EventBody::ServiceLink {
            service: name.into(),
            link,
        });
    }
}

/// The first capture group of `re` in `line`, if it is an `http://` or
/// `https://` address.
fn link_in(re: &Regex, line: &str) -> Option<String> {
    let link = re.captures(line)?.get(1)?.as_str().trim();
    let lower = link.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")).then(|| link.to_string())
}

/// A link to the instance's own port on this machine, moved to the
/// service's port: behind the proxy (or a dev server's forward) the
/// instance listens on an internal port that goes with it. `0.0.0.0`, which
/// a server prints for "every address", becomes `localhost`.
fn on_service_port(link: &str, instance_port: Option<u16>, port: Option<u16>) -> String {
    let Some(i) = link.find("://") else {
        return link.to_string();
    };
    let (scheme, rest) = link.split_at(i + 3);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    // No user part: a link with credentials stays as it is.
    if authority.contains('@') {
        return link.to_string();
    }
    let (host, at) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !p.contains(']') => (h, p.parse::<u16>().ok()),
        _ => (authority, None),
    };
    let local = matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "[::]"
    );
    if !local {
        return link.to_string();
    }
    let host = match host {
        "0.0.0.0" | "[::]" => "localhost",
        h => h,
    };
    let at = match (at, instance_port, port) {
        (Some(a), Some(i), Some(p)) if a == i => Some(p),
        (a, _, _) => a,
    };
    match at {
        Some(p) => format!("{scheme}{host}:{p}{tail}"),
        None => format!("{scheme}{host}{tail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_capture_group_is_the_link() {
        let re = Regex::new(r"app ui: (\S+)").unwrap();
        assert_eq!(
            link_in(&re, "app ui: http://127.0.0.1:5100/#token=abc").as_deref(),
            Some("http://127.0.0.1:5100/#token=abc")
        );
        assert_eq!(link_in(&re, "something else"), None);
        // No web address: not a link the dashboard opens.
        assert_eq!(link_in(&re, "app ui: file:///etc/passwd"), None);
    }

    #[test]
    fn the_instance_port_becomes_the_service_port() {
        let moved = |l: &str| on_service_port(l, Some(41234), Some(8888));
        assert_eq!(
            moved("http://127.0.0.1:41234/lab?token=abc"),
            "http://127.0.0.1:8888/lab?token=abc"
        );
        assert_eq!(moved("http://0.0.0.0:41234/"), "http://localhost:8888/");
        assert_eq!(moved("http://[::1]:41234#t=1"), "http://[::1]:8888#t=1");
        // Another port, another host, credentials: as printed.
        assert_eq!(moved("http://localhost:9000/x"), "http://localhost:9000/x");
        assert_eq!(
            moved("http://example.test:41234/"),
            "http://example.test:41234/"
        );
        assert_eq!(
            moved("http://u:p@localhost:41234/"),
            "http://u:p@localhost:41234/"
        );
        // Without a proxy the instance runs on the service's port.
        assert_eq!(
            on_service_port("http://localhost:8888/?token=a", Some(8888), Some(8888)),
            "http://localhost:8888/?token=a"
        );
        assert_eq!(
            on_service_port("http://localhost:5000/", Some(5000), None),
            "http://localhost:5000/"
        );
    }
}
