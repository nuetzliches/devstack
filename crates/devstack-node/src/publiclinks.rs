//! Public links through the pool (ADR-0090, step 3): the coordinator holds
//! a Cloudflare API token for one zone, gives each member machine a tunnel
//! of its own, and keeps a DNS record for each link a member has open, at
//! `{service}-{lane}-{stack}--{member}.{domain}`.
//!
//! The token is read from `DEVSTACK_CLOUDFLARE_TOKEN`, else
//! `cloudflare.token` in the coordinator's directory, as the provisioner's
//! (ADR-0051); no route takes or hands it out. It needs *Cloudflare Tunnel:
//! Edit* and *DNS: Edit* (and *Zone: Read*, unless the settings name the
//! zone and the account), and *Access: Apps and Policies: Read* for the
//! mode `access`. `DEVSTACK_CLOUDFLARE_API` points the coordinator at
//! another API (a test's fake).
//!
//! A member's hub reports the hosts it carries whenever they change and
//! every minute; the coordinator makes the records it lacks, removes those
//! no longer reported, and all of a member's once it has not reported for
//! [`SILENT`]. An admin can close a link: its record goes, and the host is
//! refused to its member until the member stops reporting it.
//!
//! A node whose own policy lets it (ADR-0092 points 6 and 7) reports the
//! hosts of its lanes' links too, with their owners' suffixes: the record
//! then points at the node's tunnel, and the owner's report of the host is
//! answered "carried by its node". Once the node no longer reports it, its
//! record goes, and the owner's next report makes it again, at the owner's.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const FILE: &str = "public-links.json";
const TOKEN_FILE: &str = "cloudflare.token";
const DEFAULT_API: &str = "https://api.cloudflare.com/client/v4";
/// A member that reported nothing for this long has its records removed.
pub const SILENT: Duration = Duration::from_secs(300);
/// How long an answer about Cloudflare Access is trusted.
const ACCESS_FOR: Duration = Duration::from_secs(60);

/// Whether the pool offers public links, and how.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// None.
    Off,
    /// Only while a Cloudflare Access application covers `*.<domain>`.
    #[default]
    Access,
    /// Without a login in front.
    Open,
}

/// What an admin sets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The zone below which links answer; none: the pool offers none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default)]
    pub mode: Mode,
    /// A member needs this tag to open links; none: every member may.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The zone's and the account's ids, if the token may not look them up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// What a member reads: whether it may have the pool's tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub domain: Option<String>,
    pub mode: Mode,
    pub allowed: bool,
    /// Why not, when not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Its hosts end in `--<suffix>.<domain>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

/// A member's tunnel, as the coordinator hands it to its hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issued {
    pub domain: String,
    pub suffix: String,
    pub tunnel_id: String,
    /// The JSON `cloudflared` takes (`AccountTag`, `TunnelID`, `TunnelSecret`).
    pub credentials: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostsReport {
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostsAnswer {
    /// Those with a record now.
    pub hosts: Vec<String>,
    /// Host -> why it has none.
    pub refused: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenLink {
    pub host: String,
    pub member: String,
    pub name: String,
    /// Unix seconds.
    pub since: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminView {
    pub settings: Settings,
    /// A token is there (never what it is).
    pub token: bool,
    /// In the mode `access`: whether an Access application covers the
    /// domain, or why that is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<Result<bool, String>>,
    pub links: Vec<OpenLink>,
    /// Hosts an admin closed, which their member still reports.
    pub blocked: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tunnel {
    tunnel_id: String,
    secret: String,
    suffix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Host {
    /// Who reports it: its owner, or the node that carries it.
    member: String,
    record: String,
    since: u64,
    /// The owner, when a node carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    carried_for: Option<String>,
}

/// Why an owner's host has no record of its: its lane's node carries it.
pub const CARRIED_BY_NODE: &str = "carried by the node its lane runs on";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    settings: Settings,
    /// Member id -> its tunnel.
    #[serde(default)]
    tunnels: BTreeMap<String, Tunnel>,
    /// Host -> its record.
    #[serde(default)]
    hosts: BTreeMap<String, Host>,
    /// Host an admin closed -> the member it was closed to.
    #[serde(default)]
    blocked: BTreeMap<String, String>,
}

/// The coordinator's side of public links.
pub struct Registry {
    dir: PathBuf,
    api: String,
    token: Option<String>,
    state: Mutex<State>,
    /// When each member last reported its hosts.
    seen: Mutex<HashMap<String, Instant>>,
    access: Mutex<Option<(Instant, Result<bool, String>)>>,
    /// One change of records at a time.
    one: Mutex<()>,
}

/// A refusal: the HTTP status and why.
pub type Refusal = (u16, String);

impl Registry {
    pub fn open(dir: &Path) -> Self {
        let token = std::env::var("DEVSTACK_CLOUDFLARE_TOKEN")
            .ok()
            .or_else(|| std::fs::read_to_string(dir.join(TOKEN_FILE)).ok())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let api = std::env::var("DEVSTACK_CLOUDFLARE_API")
            .ok()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| DEFAULT_API.into());
        Self::with(dir, &api, token)
    }

    /// [`Self::open`] with the API and the token given.
    pub fn with(dir: &Path, api: &str, token: Option<String>) -> Self {
        let state: State = std::fs::read(dir.join(FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        // A coordinator that starts again gives its members a full
        // [`SILENT`] to report before their records go.
        let now = Instant::now();
        let seen = state
            .hosts
            .values()
            .map(|h| (h.member.clone(), now))
            .collect();
        Self {
            dir: dir.to_path_buf(),
            api: api.trim_end_matches('/').to_string(),
            token,
            state: Mutex::new(state),
            seen: Mutex::new(seen),
            access: Mutex::new(None),
            one: Mutex::new(()),
        }
    }

    fn save(&self, s: &State) {
        let text = serde_json::to_string_pretty(s).unwrap_or_default();
        // The tunnels' secrets are in it.
        let _ = crate::pki::write_private(&self.dir.join(FILE), &text);
    }

    pub fn settings(&self) -> Settings {
        self.state.lock().unwrap().settings.clone()
    }

    /// Set what the admin chose; a changed domain takes the old records
    /// and tunnels with it.
    pub fn set_settings(&self, mut new: Settings) -> Result<(), Refusal> {
        if let Some(d) = &new.domain {
            let d = d.trim().trim_start_matches("*.").to_ascii_lowercase();
            if !valid_domain(&d) {
                return Err((400, format!("`{d}` is no domain")));
            }
            new.domain = Some(d);
        }
        new.tag = new
            .tag
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let _one = self.one.lock().unwrap();
        let old = self.settings();
        if old.domain != new.domain {
            // Records of another domain are not this one's.
            self.remove_all_records();
            let mut s = self.state.lock().unwrap();
            s.tunnels.clear();
            s.blocked.clear();
        }
        let mut s = self.state.lock().unwrap();
        s.settings = new;
        self.save(&s);
        *self.access.lock().unwrap() = None;
        Ok(())
    }

    fn allowed(&self, s: &Settings, tags: &[String]) -> Result<String, String> {
        let domain = self.offered(s)?;
        if let Some(tag) = &s.tag
            && !tags.iter().any(|t| t == tag)
        {
            return Err(format!(
                "the pool's public links are for members tagged `{tag}`"
            ));
        }
        Ok(domain)
    }

    /// The pool offers public links at all: its domain. A node carries
    /// its lanes' owners' links whatever its own tags.
    fn offered(&self, s: &Settings) -> Result<String, String> {
        let Some(domain) = s.domain.clone() else {
            return Err("the pool offers no public links".into());
        };
        if s.mode == Mode::Off {
            return Err("an admin switched the pool's public links off".into());
        }
        if self.token.is_none() {
            return Err("the coordinator has no Cloudflare token (`cloudflare.token`)".into());
        }
        Ok(domain)
    }

    /// The suffix of a member that has a tunnel of the pool.
    pub fn suffix_of(&self, member: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .tunnels
            .get(member)
            .map(|t| t.suffix.clone())
    }
    /// What `member` reads of the pool's public links.
    pub fn offer(&self, member: &str, name: &str, tags: &[String]) -> Offer {
        let s = self.settings();
        let allowed = self.allowed(&s, tags).and_then(|_| self.access_ok(&s));
        Offer {
            domain: s.domain.clone(),
            mode: s.mode,
            allowed: allowed.is_ok(),
            why: allowed.err(),
            suffix: s.domain.as_ref().map(|_| self.suffix_for(member, name)),
        }
    }

    /// In the mode `access`, an Access application covers the domain.
    fn access_ok(&self, s: &Settings) -> Result<(), String> {
        if s.mode != Mode::Access {
            return Ok(());
        }
        let known = self
            .access
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(at, _)| at.elapsed() < ACCESS_FOR)
            .map(|(_, r)| r.clone());
        let r = match known {
            Some(r) => r,
            None => {
                let r = self.find_access(s);
                *self.access.lock().unwrap() = Some((Instant::now(), r.clone()));
                r
            }
        };
        match r {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!(
                "the pool's links need a login in front, and no Cloudflare Access application covers `*.{}` yet",
                s.domain.as_deref().unwrap_or("")
            )),
            Err(e) => Err(format!(
                "whether Cloudflare Access covers the domain is not known: {e}"
            )),
        }
    }

    fn find_access(&self, s: &Settings) -> Result<bool, String> {
        let domain = s.domain.as_deref().unwrap_or("");
        let (_, account) = self.zone(s)?;
        let apps = self.cf("GET", &format!("/accounts/{account}/access/apps"), None)?;
        let wild = format!("*.{domain}");
        Ok(apps.as_array().into_iter().flatten().any(|a| {
            a["domain"] == wild.as_str()
                || a["self_hosted_domains"]
                    .as_array()
                    .is_some_and(|d| d.iter().any(|x| x == wild.as_str()))
        }))
    }

    /// The member's host suffix: its name as a DNS label, made unique.
    fn suffix_for(&self, member: &str, name: &str) -> String {
        let s = self.state.lock().unwrap();
        if let Some(t) = s.tunnels.get(member) {
            return t.suffix.clone();
        }
        let mut base = label_part(name);
        if base.is_empty() {
            base = member.chars().take(8).collect();
        }
        base.truncate(20);
        let taken = |x: &str| s.tunnels.iter().any(|(m, t)| m != member && t.suffix == x);
        if taken(&base) {
            format!("{base}-{}", member.chars().take(4).collect::<String>())
        } else {
            base
        }
    }

    /// The member's tunnel, made at its first request.
    /// `as_node`: a node, for its lanes' links (no tag needed).
    pub fn issue(
        &self,
        member: &str,
        name: &str,
        tags: &[String],
        as_node: bool,
    ) -> Result<Issued, Refusal> {
        let settings = self.settings();
        let domain = if as_node {
            self.offered(&settings)
        } else {
            self.allowed(&settings, tags)
        }
        .map_err(|e| (403, e))?;
        self.access_ok(&settings).map_err(|e| (403, e))?;
        let _one = self.one.lock().unwrap();
        let suffix = self.suffix_for(member, name);
        let existing = self.state.lock().unwrap().tunnels.get(member).cloned();
        let (_, account) = self.zone(&settings).map_err(|e| (502, e))?;
        let tunnel = match existing {
            Some(t) => t,
            None => {
                let secret = random_secret();
                let made = self
                    .cf(
                        "POST",
                        &format!("/accounts/{account}/cfd_tunnel"),
                        Some(json!({
                            "name": format!("devstack-{suffix}-{}", &member.chars().take(8).collect::<String>()),
                            "tunnel_secret": secret,
                            "config_src": "local",
                        })),
                    )
                    .map_err(|e| (502, e))?;
                let tunnel_id = made["id"]
                    .as_str()
                    .ok_or((502, "Cloudflare made a tunnel without an id".to_string()))?
                    .to_string();
                let t = Tunnel {
                    tunnel_id,
                    secret,
                    suffix: suffix.clone(),
                };
                let mut s = self.state.lock().unwrap();
                s.tunnels.insert(member.to_string(), t.clone());
                self.save(&s);
                t
            }
        };
        Ok(Issued {
            domain,
            suffix: tunnel.suffix.clone(),
            tunnel_id: tunnel.tunnel_id.clone(),
            credentials: json!({
                "AccountTag": account,
                "TunnelID": tunnel.tunnel_id,
                "TunnelSecret": tunnel.secret,
            })
            .to_string(),
        })
    }

    /// The hosts a member carries now: records made for the new ones,
    /// removed for those it no longer reports. `carries_for`: a node's
    /// lanes' owners, by their suffix, whose hosts it may carry.
    pub fn report(
        &self,
        member: &str,
        tags: &[String],
        hosts: &[String],
        carries_for: &BTreeMap<String, String>,
    ) -> HostsAnswer {
        self.seen
            .lock()
            .unwrap()
            .insert(member.to_string(), Instant::now());
        let settings = self.settings();
        let mut answer = HostsAnswer::default();
        let reported: BTreeSet<String> = hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
        let _one = self.one.lock().unwrap();
        // What it no longer reports goes; a host an admin closed is free
        // again once its member stops reporting it.
        let gone: Vec<(String, String)> = {
            let mut s = self.state.lock().unwrap();
            let before = s.blocked.len();
            s.blocked.retain(|h, m| m != member || reported.contains(h));
            if s.blocked.len() != before {
                self.save(&s);
            }
            s.hosts
                .iter()
                .filter(|(h, r)| r.member == member && !reported.contains(*h))
                .map(|(h, r)| (h.clone(), r.record.clone()))
                .collect()
        };
        for (host, record) in gone {
            self.remove_record(&settings, &host, &record);
        }
        let allowed = if carries_for.is_empty() {
            self.allowed(&settings, tags)
        } else {
            self.offered(&settings)
        }
        .and_then(|d| self.access_ok(&settings).map(|_| d));
        let Some((tunnel, suffix)) = self
            .state
            .lock()
            .unwrap()
            .tunnels
            .get(member)
            .map(|t| (t.tunnel_id.clone(), t.suffix.clone()))
        else {
            for h in reported {
                answer
                    .refused
                    .insert(h, "the pool gave this machine no tunnel yet".into());
            }
            return answer;
        };
        for host in reported {
            // A node's host of an owner it carries for: that owner's suffix.
            let owner = carries_for.iter().find_map(|(sfx, owner)| {
                host.strip_suffix(&format!(
                    "--{sfx}.{}",
                    settings.domain.as_deref().unwrap_or("")
                ))
                .map(|_| (sfx.clone(), owner.clone()))
            });
            let why = match (&allowed, &owner) {
                (Err(e), _) => Some(e.clone()),
                (Ok(domain), Some((sfx, _))) => host_refused(&host, domain, sfx),
                (Ok(domain), None) => host_refused(&host, domain, &suffix),
            };
            let held = self.state.lock().unwrap().hosts.get(&host).cloned();
            let why = why.or_else(|| {
                if self.state.lock().unwrap().blocked.contains_key(&host) {
                    return Some("an admin of the pool closed this link".into());
                }
                match (&held, &owner) {
                    (Some(r), None) if r.carried_for.as_deref() == Some(member) => {
                        Some(CARRIED_BY_NODE.to_string())
                    }
                    // A node takes over its owner's record.
                    (Some(r), Some((_, o))) if r.member == *o || r.member == member => None,
                    (Some(r), _) if r.member != member => {
                        Some("another member has this address".to_string())
                    }
                    _ => None,
                }
            });
            if let Some(why) = why {
                answer.refused.insert(host, why);
                continue;
            }
            match held {
                Some(r) if r.member == member => {
                    answer.hosts.push(host);
                    continue;
                }
                // The owner's record, the node's now.
                Some(r) => {
                    if let Err(e) = self.point_record(&settings, &r.record, &host, &tunnel) {
                        answer.refused.insert(host, format!("Cloudflare: {e}"));
                        continue;
                    }
                    let mut s = self.state.lock().unwrap();
                    s.hosts.insert(
                        host.clone(),
                        Host {
                            member: member.to_string(),
                            record: r.record,
                            since: now_secs(),
                            carried_for: owner.map(|(_, o)| o),
                        },
                    );
                    self.save(&s);
                    answer.hosts.push(host);
                    continue;
                }
                None => {}
            }
            match self.make_record(&settings, &host, &tunnel) {
                Ok(record) => {
                    let mut s = self.state.lock().unwrap();
                    s.hosts.insert(
                        host.clone(),
                        Host {
                            member: member.to_string(),
                            record,
                            since: now_secs(),
                            carried_for: owner.map(|(_, o)| o),
                        },
                    );
                    self.save(&s);
                    answer.hosts.push(host);
                }
                Err(e) => {
                    answer.refused.insert(host, format!("Cloudflare: {e}"));
                }
            }
        }
        answer
    }

    /// Remove the records of members that went silent.
    pub fn sweep(&self) {
        let silent: BTreeSet<String> = {
            let seen = self.seen.lock().unwrap();
            let s = self.state.lock().unwrap();
            s.hosts
                .values()
                .map(|h| h.member.clone())
                .filter(|m| seen.get(m).is_none_or(|t| t.elapsed() > SILENT))
                .collect()
        };
        if silent.is_empty() {
            return;
        }
        let settings = self.settings();
        let _one = self.one.lock().unwrap();
        let gone: Vec<(String, String)> = self
            .state
            .lock()
            .unwrap()
            .hosts
            .iter()
            .filter(|(_, h)| silent.contains(&h.member))
            .map(|(k, h)| (k.clone(), h.record.clone()))
            .collect();
        for (host, record) in gone {
            self.remove_record(&settings, &host, &record);
        }
    }

    /// An admin closes a link: its record goes, its member is refused it
    /// until it stops reporting it.
    pub fn close(&self, host: &str) -> Result<(), Refusal> {
        let host = host.to_ascii_lowercase();
        let settings = self.settings();
        let _one = self.one.lock().unwrap();
        let (member, record) = self
            .state
            .lock()
            .unwrap()
            .hosts
            .get(&host)
            .map(|h| (h.member.clone(), h.record.clone()))
            .ok_or((404, format!("no public link `{host}` is open")))?;
        self.remove_record(&settings, &host, &record);
        let mut s = self.state.lock().unwrap();
        s.blocked.insert(host, member);
        self.save(&s);
        Ok(())
    }

    pub fn admin_view(&self, names: &HashMap<String, String>) -> AdminView {
        let settings = self.settings();
        let access =
            (settings.mode == Mode::Access && settings.domain.is_some() && self.token.is_some())
                .then(|| match self.access_ok(&settings) {
                    Ok(()) => Ok(true),
                    Err(e) if e.contains("no Cloudflare Access application") => Ok(false),
                    Err(e) => Err(e),
                });
        let s = self.state.lock().unwrap();
        AdminView {
            settings: s.settings.clone(),
            token: self.token.is_some(),
            access,
            links: s
                .hosts
                .iter()
                .map(|(host, h)| OpenLink {
                    host: host.clone(),
                    member: h.member.clone(),
                    name: names.get(&h.member).cloned().unwrap_or_default(),
                    since: h.since,
                })
                .collect(),
            blocked: s.blocked.keys().cloned().collect(),
        }
    }

    /// A member removed from the pool: its records go, and its tunnel is
    /// forgotten (Cloudflare keeps an unused tunnel; deleting it needs its
    /// connections gone first).
    pub fn forget(&self, member: &str) {
        let settings = self.settings();
        let _one = self.one.lock().unwrap();
        let gone: Vec<(String, String)> = self
            .state
            .lock()
            .unwrap()
            .hosts
            .iter()
            .filter(|(_, h)| h.member == member)
            .map(|(k, h)| (k.clone(), h.record.clone()))
            .collect();
        for (host, record) in gone {
            self.remove_record(&settings, &host, &record);
        }
        let mut s = self.state.lock().unwrap();
        s.tunnels.remove(member);
        s.blocked.retain(|_, m| m != member);
        self.save(&s);
    }

    fn remove_all_records(&self) {
        let settings = self.settings();
        let gone: Vec<(String, String)> = self
            .state
            .lock()
            .unwrap()
            .hosts
            .iter()
            .map(|(k, h)| (k.clone(), h.record.clone()))
            .collect();
        for (host, record) in gone {
            self.remove_record(&settings, &host, &record);
        }
    }

    fn remove_record(&self, settings: &Settings, host: &str, record: &str) {
        if let Ok((zone, _)) = self.zone(settings) {
            // Gone already is gone.
            let _ = self.cf(
                "DELETE",
                &format!("/zones/{zone}/dns_records/{record}"),
                None,
            );
        }
        let mut s = self.state.lock().unwrap();
        s.hosts.remove(host);
        self.save(&s);
    }

    fn make_record(&self, settings: &Settings, host: &str, tunnel: &str) -> Result<String, String> {
        let (zone, _) = self.zone(settings)?;
        let body = json!({
            "type": "CNAME",
            "name": host,
            "content": format!("{tunnel}.cfargotunnel.com"),
            "proxied": true,
            "comment": "devstack public link",
        });
        match self.cf(
            "POST",
            &format!("/zones/{zone}/dns_records"),
            Some(body.clone()),
        ) {
            Ok(r) => r["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "a record without an id".into()),
            // One of that name exists (left from before): it is taken over.
            Err(e) if e.contains("81053") || e.contains("already exists") => {
                let found = self.cf(
                    "GET",
                    &format!("/zones/{zone}/dns_records?name={host}"),
                    None,
                )?;
                let id = found
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|r| r["id"].as_str())
                    .ok_or(e)?
                    .to_string();
                self.cf(
                    "PUT",
                    &format!("/zones/{zone}/dns_records/{id}"),
                    Some(body),
                )?;
                Ok(id)
            }
            Err(e) => Err(e),
        }
    }

    /// Point an existing record at another tunnel.
    fn point_record(
        &self,
        settings: &Settings,
        record: &str,
        host: &str,
        tunnel: &str,
    ) -> Result<(), String> {
        let (zone, _) = self.zone(settings)?;
        self.cf(
            "PUT",
            &format!("/zones/{zone}/dns_records/{record}"),
            Some(json!({
                "type": "CNAME",
                "name": host,
                "content": format!("{tunnel}.cfargotunnel.com"),
                "proxied": true,
                "comment": "devstack public link",
            })),
        )
        .map(|_| ())
    }

    /// The zone's id and its account's.
    fn zone(&self, settings: &Settings) -> Result<(String, String), String> {
        if let (Some(z), Some(a)) = (&settings.zone_id, &settings.account_id) {
            return Ok((z.clone(), a.clone()));
        }
        let domain = settings.domain.as_deref().ok_or("no domain set")?;
        let zones = self.cf("GET", &format!("/zones?name={domain}"), None)?;
        let z = zones
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| format!("the token sees no zone `{domain}`"))?;
        match (z["id"].as_str(), z["account"]["id"].as_str()) {
            (Some(id), Some(acct)) => Ok((id.to_string(), acct.to_string())),
            _ => Err(format!("Cloudflare named zone `{domain}` without its ids")),
        }
    }

    /// One call of Cloudflare's API: its `result`, or its first error.
    fn cf(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let token = self.token.as_deref().ok_or("no Cloudflare token")?;
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let url = format!("{}{path}", self.api);
        let auth = format!("Bearer {token}");
        let sent = match (method, body) {
            ("GET", _) => agent.get(&url).header("Authorization", &auth).call(),
            ("DELETE", _) => agent.delete(&url).header("Authorization", &auth).call(),
            ("POST", Some(b)) => agent
                .post(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(b.to_string()),
            ("PUT", Some(b)) => agent
                .put(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(b.to_string()),
            _ => return Err(format!("unsupported call {method} {path}")),
        };
        let mut resp = sent.map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if v["success"] == true {
            return Ok(v["result"].clone());
        }
        let e = &v["errors"][0];
        Err(match (e["code"].as_u64(), e["message"].as_str()) {
            (Some(code), Some(m)) => format!("{m} ({code})"),
            _ => format!("{method} {path} answered {status}"),
        })
    }
}

/// Why a member may not have `host`, if it may not: it must be one label
/// below the domain, ending in the member's suffix.
pub fn host_refused(host: &str, domain: &str, suffix: &str) -> Option<String> {
    let Some(label) = host.strip_suffix(&format!(".{domain}")) else {
        return Some(format!("not below {domain}"));
    };
    if !label.ends_with(&format!("--{suffix}")) {
        return Some(format!(
            "this machine's addresses end in --{suffix}.{domain}"
        ));
    }
    if label.len() > 63
        || !label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Some("not a DNS label".into());
    }
    None
}

/// A name as part of a DNS label: lowercase, anything but letters and
/// digits one hyphen (as the daemon names hosts).
pub fn label_part(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn valid_domain(d: &str) -> bool {
    let labels: Vec<&str> = d.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// A tunnel's secret: 32 random bytes, base64.
fn random_secret() -> String {
    use base64::Engine;
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cloudflare's API as far as the registry calls it, its DNS records
    /// kept in `records`.
    fn fake_cloudflare(access: bool) -> (String, std::sync::Arc<Mutex<BTreeMap<String, String>>>) {
        use axum::extract::{Path as P, State};
        use axum::routing::{delete, get, post};
        type Records = std::sync::Arc<Mutex<BTreeMap<String, String>>>;
        let records: Records = Default::default();
        fn ok(result: Value) -> axum::Json<Value> {
            axum::Json(json!({ "success": true, "errors": [], "result": result }))
        }
        let app = axum::Router::new()
            .route(
                "/zones",
                get(|| async { ok(json!([{ "id": "z1", "account": { "id": "a1" } }])) }),
            )
            .route(
                "/accounts/a1/cfd_tunnel",
                post(|| async { ok(json!({ "id": "tun1" })) }),
            )
            .route(
                "/accounts/a1/access/apps",
                get(move || async move {
                    ok(if access {
                        json!([{ "domain": "*.dev.link" }])
                    } else {
                        json!([])
                    })
                }),
            )
            .route(
                "/zones/z1/dns_records",
                post(
                    |State(r): State<Records>, axum::Json(b): axum::Json<Value>| async move {
                        let name = b["name"].as_str().unwrap().to_string();
                        assert_eq!(b["content"], "tun1.cfargotunnel.com");
                        let id = format!("r-{name}");
                        r.lock().unwrap().insert(id.clone(), name);
                        ok(json!({ "id": id }))
                    },
                ),
            )
            .route(
                "/zones/z1/dns_records/{id}",
                delete(|State(r): State<Records>, P(id): P<String>| async move {
                    r.lock().unwrap().remove(&id);
                    ok(json!({ "id": id }))
                })
                .put(
                    |State(r): State<Records>,
                     P(id): P<String>,
                     axum::Json(b): axum::Json<Value>| async move {
                        r.lock()
                            .unwrap()
                            .insert(id.clone(), b["name"].as_str().unwrap_or("").to_string());
                        ok(json!({ "id": id }))
                    },
                ),
            )
            .with_state(records.clone());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let l = tokio::net::TcpListener::from_std(listener).unwrap();
                    axum::serve(l, app).await.unwrap();
                })
        });
        (url, records)
    }

    #[test]
    fn a_member_gets_a_tunnel_and_records_for_its_own_open_links() {
        let d = tempfile::tempdir().unwrap();
        let (api, records) = fake_cloudflare(true);
        let r = Registry::with(d.path(), &api, Some("token".into()));
        let tags = vec!["team".to_string()];
        assert!(!r.offer("aaaa1111", "Anna", &tags).allowed, "no domain yet");
        r.set_settings(Settings {
            domain: Some("dev.link".into()),
            tag: Some("team".into()),
            ..Default::default()
        })
        .unwrap();
        let offer = r.offer("aaaa1111", "Anna", &tags);
        assert!(offer.allowed, "{offer:?}");
        assert_eq!(offer.suffix.as_deref(), Some("anna"));
        assert!(!r.offer("bbbb2222", "Bert", &[]).allowed, "untagged");

        let none = BTreeMap::new();
        let issued = r.issue("aaaa1111", "Anna", &tags, false).unwrap();
        assert_eq!(
            (issued.tunnel_id.as_str(), issued.suffix.as_str()),
            ("tun1", "anna")
        );
        let creds: Value = serde_json::from_str(&issued.credentials).unwrap();
        assert_eq!(creds["AccountTag"], "a1");
        assert!(creds["TunnelSecret"].as_str().is_some_and(|s| s.len() > 30));
        // The secret is kept privately, the token never.
        let file = std::fs::read_to_string(d.path().join(FILE)).unwrap();
        assert!(!file.contains("\"token\""), "{file}");

        let a = r.report(
            "aaaa1111",
            &tags,
            &[
                "web-shop--anna.dev.link".into(),
                "web-shop--bert.dev.link".into(),
            ],
            &none,
        );
        assert_eq!(a.hosts, vec!["web-shop--anna.dev.link".to_string()]);
        assert!(a.refused.contains_key("web-shop--bert.dev.link"), "{a:?}");
        assert_eq!(records.lock().unwrap().len(), 1);
        // No longer reported: its record goes.
        r.report("aaaa1111", &tags, &[], &none);
        assert!(records.lock().unwrap().is_empty());

        // An admin closes one: refused until its member stops reporting it.
        r.report(
            "aaaa1111",
            &tags,
            &["api-shop--anna.dev.link".into()],
            &none,
        );
        r.close("api-shop--anna.dev.link").unwrap();
        assert!(records.lock().unwrap().is_empty());
        // Another member's report does not open it again.
        r.report("bbbb2222", &tags, &[], &none);
        let a = r.report(
            "aaaa1111",
            &tags,
            &["api-shop--anna.dev.link".into()],
            &none,
        );
        assert!(
            a.hosts.is_empty() && a.refused.contains_key("api-shop--anna.dev.link"),
            "{a:?}"
        );
        r.report("aaaa1111", &tags, &[], &none);
        let a = r.report(
            "aaaa1111",
            &tags,
            &["api-shop--anna.dev.link".into()],
            &none,
        );
        assert_eq!(a.hosts.len(), 1, "{a:?}");
        // A coordinator that starts again keeps the records a while.
        let again = Registry::with(d.path(), &api, Some("token".into()));
        again.sweep();
        assert_eq!(records.lock().unwrap().len(), 1);
        // Its lane's node carries the host: the record is the node's, the
        // owner's report is told so; the node stops, the owner's is again.
        let carries: BTreeMap<String, String> =
            [("anna".to_string(), "aaaa1111".to_string())].into();
        r.issue("nnnn9999", "build", &[], true).unwrap();
        let a = r.report(
            "nnnn9999",
            &[],
            &["api-shop--anna.dev.link".into(), "x--bert.dev.link".into()],
            &carries,
        );
        assert_eq!(
            a.hosts,
            vec!["api-shop--anna.dev.link".to_string()],
            "{a:?}"
        );
        assert!(
            a.refused.contains_key("x--bert.dev.link"),
            "not its owner's"
        );
        let a = r.report(
            "aaaa1111",
            &tags,
            &["api-shop--anna.dev.link".into()],
            &none,
        );
        assert_eq!(
            a.refused.get("api-shop--anna.dev.link").map(String::as_str),
            Some(CARRIED_BY_NODE)
        );
        r.report("nnnn9999", &[], &[], &carries);
        let a = r.report(
            "aaaa1111",
            &tags,
            &["api-shop--anna.dev.link".into()],
            &none,
        );
        assert_eq!(a.hosts.len(), 1, "{a:?}");
        // The member leaves the pool: its records go.
        r.forget("aaaa1111");
        assert!(records.lock().unwrap().is_empty());
    }

    #[test]
    fn the_mode_access_needs_an_access_application() {
        let d = tempfile::tempdir().unwrap();
        let (api, _) = fake_cloudflare(false);
        let r = Registry::with(d.path(), &api, Some("token".into()));
        r.set_settings(Settings {
            domain: Some("dev.link".into()),
            ..Default::default()
        })
        .unwrap();
        let offer = r.offer("aaaa1111", "Anna", &[]);
        assert!(
            !offer.allowed && offer.why.as_deref().is_some_and(|w| w.contains("Access")),
            "{offer:?}"
        );
        assert_eq!(r.issue("aaaa1111", "Anna", &[], false).unwrap_err().0, 403);
        r.set_settings(Settings {
            domain: Some("dev.link".into()),
            mode: Mode::Open,
            ..Default::default()
        })
        .unwrap();
        assert!(r.offer("aaaa1111", "Anna", &[]).allowed);
    }

    #[test]
    fn a_member_has_only_its_own_hosts() {
        assert_eq!(
            host_refused("web-shop--anna.dev.link", "dev.link", "anna"),
            None
        );
        assert!(host_refused("web-shop--bert.dev.link", "dev.link", "anna").is_some());
        assert!(host_refused("web-shop--anna.other.link", "dev.link", "anna").is_some());
        assert!(host_refused("x.web-shop--anna.dev.link", "dev.link", "anna").is_some());
        assert!(host_refused("web-shop-anna.dev.link", "dev.link", "anna").is_some());
    }

    #[test]
    fn a_suffix_is_unique_in_the_pool() {
        let d = tempfile::tempdir().unwrap();
        let r = Registry::open(d.path());
        r.state.lock().unwrap().tunnels.insert(
            "aaaa1111".into(),
            Tunnel {
                tunnel_id: "t".into(),
                secret: "s".into(),
                suffix: "anna".into(),
            },
        );
        assert_eq!(r.suffix_for("aaaa1111", "Anna"), "anna");
        assert_eq!(r.suffix_for("bbbb2222", "Anna"), "anna-bbbb");
        assert_eq!(r.suffix_for("cccc3333", "Bert's Laptop"), "bert-s-laptop");
    }
}
