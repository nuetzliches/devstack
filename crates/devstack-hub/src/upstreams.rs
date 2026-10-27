//! MCP upstreams (ADR-0069; review card on #316, approved 2026-10-27): the
//! MCP servers a user configured once in the hub, logged in once by the hub,
//! whose tools `devstack mcp` offers beside its own (that part follows).
//!
//! - **Configuration** per user in `upstreams.toml` in the hub's state
//!   directory: name, URL, prefix, tool allowlist, how it logs in. No secret
//!   in it (card point 4).
//! - **Secrets** in [`crate::secrets`]: a static token the user pasted, or
//!   the OAuth tokens of the hub's own login.
//! - **OAuth** (card point 1): the hub is the only client of an upstream.
//!   It finds the authorization server from the upstream (RFC 9728, then
//!   RFC 8414), registers itself where the server allows it (RFC 7591),
//!   and runs the authorization code flow with PKCE (S256) and the
//!   upstream as `resource` (RFC 8707). The browser comes back to the hub's
//!   own loopback port; a callback is taken only for a flow the hub started,
//!   by its one-time `state`, within ten minutes. No route hands a token out.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::secrets::Secrets;

/// The path the authorization server sends the browser back to, outside
/// `/hub/`: that navigation carries no session (the cookie is `Strict`), so
/// the flow's `state` is what admits it.
pub const CALLBACK: &str = "/oauth/upstream/callback";

/// How long a started login waits for its callback.
const FLOW_FOR: Duration = Duration::from_secs(600);

/// One upstream as the user configured it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Upstream {
    /// Its name in the dashboard and in the keychain: `[a-z0-9-]`, 1 to 32.
    pub name: String,
    /// Its MCP endpoint (Streamable HTTP); empty for one that is a command.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// The command the hub starts for it instead (stdio, card point 3): the
    /// program and its arguments, as the user entered them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// The variables the command gets besides a base to run at all (no
    /// secret: the configuration is not the keychain).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// For a command logging in with a token the hub keeps: the variable it
    /// is given in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_var: Option<String>,
    /// What its tools are prefixed with (`<prefix>__<tool>`); the name with
    /// `_` for `-` when not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// The tools offered; empty: all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default)]
    pub auth: Auth,
}

/// How an upstream logs in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Auth {
    /// No credential.
    #[default]
    None,
    /// A static bearer token the user gives the hub (kept in the keychain),
    /// or one from the environment variable `env` (a machine without a
    /// browser, ADR-0069 point 6).
    Token {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<String>,
    },
    /// The hub's OAuth login. `client_id` where the authorization server
    /// does not register clients itself.
    Oauth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        scopes: Vec<String>,
    },
}

impl Upstream {
    /// It is a command the hub starts (stdio), not a URL.
    pub fn is_command(&self) -> bool {
        !self.command.is_empty()
    }

    pub fn prefix(&self) -> String {
        self.prefix
            .clone()
            .unwrap_or_else(|| self.name.replace('-', "_"))
    }

    /// What is wrong with it, if anything.
    pub fn check(&self) -> Result<(), String> {
        let n = &self.name;
        if n.is_empty()
            || n.len() > 32
            || !n
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!("`{n}`: a name is 1 to 32 of a-z, 0-9 and -"));
        }
        let p = self.prefix();
        if p.is_empty()
            || p.len() > 32
            || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(format!("`{p}`: a prefix is 1 to 32 of a-z, 0-9 and _"));
        }
        let var =
            |e: &str| !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if self.is_command() {
            if !self.url.trim().is_empty() {
                return Err("an upstream is a URL or a command, not both".into());
            }
            if self.command[0].trim().is_empty() {
                return Err("the command names no program".into());
            }
            if let Some(k) = self.env.keys().find(|k| !var(k)) {
                return Err(format!("`{k}`: not the name of an environment variable"));
            }
            if matches!(self.auth, Auth::Oauth { .. }) {
                return Err("a command logs in with a token, not with OAuth".into());
            }
            match (&self.auth, &self.token_var) {
                (Auth::Token { env: None }, Some(t)) if var(t) => {}
                (Auth::Token { env: None }, _) => {
                    return Err(
                        "a command with a token the hub keeps needs the variable it is given in"
                            .into(),
                    );
                }
                (_, Some(_)) => {
                    return Err("`token_var` is for a command with a token the hub keeps".into());
                }
                _ => {}
            }
        } else {
            let u = self.url.trim();
            let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
                .iter()
                .any(|l| u.starts_with(l));
            if !(u.starts_with("https://") || local) {
                return Err(format!(
                    "`{u}`: an upstream is reached over https (plain http on this machine only)"
                ));
            }
            if !self.env.is_empty() || self.token_var.is_some() {
                return Err("`env` and `token_var` are for an upstream that is a command".into());
            }
        }
        if let Auth::Token { env: Some(e) } = &self.auth
            && (e.is_empty() || !e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        {
            return Err(format!("`{e}`: not the name of an environment variable"));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "upstream")]
    upstreams: Vec<Upstream>,
}

/// What the hub keeps of an upstream's login in the secrets store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Secret {
    Token {
        token: String,
    },
    Oauth {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        /// Unix seconds.
        #[serde(default)]
        expires_at: Option<u64>,
        client_id: String,
        token_endpoint: String,
        resource: String,
    },
}

/// When an upstream was asked for its tools, and what it answered.
type Listed = (Instant, Result<Vec<Value>, String>);

/// A login started and not come back yet.
struct Flow {
    upstream: String,
    verifier: String,
    client_id: String,
    token_endpoint: String,
    resource: String,
    redirect: String,
    started: Instant,
}

/// The upstreams of this hub: their configuration, their secrets, the
/// logins under way.
pub struct Upstreams {
    file: PathBuf,
    secrets: Secrets,
    flows: Mutex<HashMap<String, Flow>>,
    /// One configuration change at a time.
    write: Mutex<()>,
    /// The MCP session with each upstream.
    sessions: Mutex<HashMap<String, crate::mcpclient::Session>>,
    /// The tools each upstream listed last, kept across restarts: a
    /// logged-out or unreachable upstream keeps them listed (card point 5).
    seen: PathBuf,
    /// The running command of each upstream that is one.
    procs: Mutex<HashMap<String, std::sync::Arc<crate::stdio::Process>>>,
    /// Where those commands' stderr goes.
    logs: PathBuf,
    /// When each upstream was asked last, and what it answered then.
    listed: Mutex<HashMap<String, Listed>>,
}

/// An upstream as the dashboard lists it: never a secret.
#[derive(Debug, Serialize)]
pub struct View {
    #[serde(flatten)]
    pub upstream: Upstream,
    pub prefix: String,
    /// It has a credential (a token, or tokens of a login); with `Auth::None`
    /// always.
    pub logged_in: bool,
}

impl Upstreams {
    pub fn open(dir: PathBuf) -> Self {
        Self::with(dir.clone(), Secrets::open(dir))
    }

    /// Its configuration without any secret: what `devstack mcp` uses without
    /// a hub, where only upstreams without a login or with a token from the
    /// environment work (ADR-0069 point 6). It keeps no tools of its own.
    pub fn without_secrets(dir: PathBuf) -> Self {
        let mut u = Self::with(dir, Secrets::nothing());
        u.seen = PathBuf::new();
        u
    }

    fn with(dir: PathBuf, secrets: Secrets) -> Self {
        Self {
            file: dir.join("upstreams.toml"),
            seen: dir.join("upstream-tools.json"),
            secrets,
            flows: Mutex::new(HashMap::new()),
            write: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            procs: Mutex::new(HashMap::new()),
            logs: dir.join("upstreams"),
            listed: Mutex::new(HashMap::new()),
        }
    }

    /// Where the secrets are: `keychain` or `file`.
    pub fn store(&self) -> &'static str {
        match self.secrets.kind() {
            crate::secrets::Kind::Keychain => "keychain",
            crate::secrets::Kind::File => "file",
        }
    }

    pub fn list(&self) -> Result<Vec<Upstream>, String> {
        let text = match std::fs::read_to_string(&self.file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("{}: {e}", self.file.display())),
        };
        toml::from_str::<File>(&text)
            .map(|f| f.upstreams)
            .map_err(|e| format!("{}: {e}", self.file.display()))
    }

    pub fn views(&self) -> Result<Vec<View>, String> {
        Ok(self
            .list()?
            .into_iter()
            .map(|u| {
                let logged_in = match &u.auth {
                    Auth::None => true,
                    Auth::Token { env: Some(e) } => std::env::var_os(e).is_some(),
                    _ => self.secrets.get(&u.name).ok().flatten().is_some(),
                };
                View {
                    prefix: u.prefix(),
                    upstream: u,
                    logged_in,
                }
            })
            .collect())
    }

    fn save(&self, all: &[Upstream]) -> Result<(), String> {
        let text = toml::to_string_pretty(&File {
            upstreams: all.to_vec(),
        })
        .map_err(|e| e.to_string())?;
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.file.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.file).map_err(|e| e.to_string())
    }

    /// Add `u`, or replace the one of its name. A change of its URL or of how
    /// it logs in forgets its credential: it was for the other.
    pub fn put(&self, u: Upstream) -> Result<(), String> {
        u.check()?;
        let name = u.name.clone();
        let _one = self.write.lock().unwrap();
        let mut all = self.list()?;
        if all
            .iter()
            .any(|o| o.name != u.name && o.prefix() == u.prefix())
        {
            return Err(format!("the prefix `{}` is another upstream's", u.prefix()));
        }
        match all.iter_mut().find(|o| o.name == u.name) {
            Some(o) => {
                if o.url != u.url
                    || o.command != u.command
                    || std::mem::discriminant(&o.auth) != std::mem::discriminant(&u.auth)
                {
                    self.secrets.delete(&u.name)?;
                }
                *o = u;
            }
            None => all.push(u),
        }
        // A command runs again as configured now.
        self.procs.lock().unwrap().remove(&name);
        self.save(&all)
    }

    pub fn remove(&self, name: &str) -> Result<bool, String> {
        let _one = self.write.lock().unwrap();
        let mut all = self.list()?;
        let before = all.len();
        all.retain(|u| u.name != name);
        if all.len() == before {
            return Ok(false);
        }
        self.procs.lock().unwrap().remove(name);
        self.secrets.delete(name)?;
        self.save(&all)?;
        Ok(true)
    }

    fn get(&self, name: &str) -> Result<Upstream, String> {
        self.list()?
            .into_iter()
            .find(|u| u.name == name)
            .ok_or_else(|| format!("no upstream `{name}`"))
    }

    /// Keep the static token of `name` (`Auth::Token` without `env`).
    pub fn set_token(&self, name: &str, token: &str) -> Result<(), String> {
        let u = self.get(name)?;
        if !matches!(u.auth, Auth::Token { env: None }) {
            return Err(format!(
                "`{name}` does not log in with a token the hub keeps"
            ));
        }
        let token = token.trim();
        if token.is_empty() {
            return Err("an empty token".into());
        }
        self.put_secret(
            name,
            &Secret::Token {
                token: token.into(),
            },
        )
    }

    pub fn logout(&self, name: &str) -> Result<(), String> {
        self.get(name)?;
        self.secrets.delete(name)
    }

    fn put_secret(&self, name: &str, s: &Secret) -> Result<(), String> {
        let text = serde_json::to_string(s).map_err(|e| e.to_string())?;
        self.secrets.set(name, &text)
    }

    /// Start the OAuth login of `name`: the authorization server found and,
    /// where it allows it, this hub registered with it. The URL to send the
    /// browser to; `redirect` is the hub's callback (`CALLBACK` on its port).
    pub fn begin(&self, http: &ureq::Agent, name: &str, redirect: &str) -> Result<String, String> {
        let u = self.get(name)?;
        let Auth::Oauth { client_id, scopes } = &u.auth else {
            return Err(format!("`{name}` does not log in with OAuth"));
        };
        let server = discover(http, &u.url)?;
        let client_id = match (client_id, &server.registration_endpoint) {
            (Some(c), _) => c.clone(),
            (None, Some(reg)) => register(http, reg, redirect)?,
            (None, None) => {
                return Err(format!(
                    "{} registers no clients itself: register one there with the redirect {redirect} and give its client id",
                    server.issuer
                ));
            }
        };
        let verifier = format!("{}{}", crate::random_hex(), crate::random_hex());
        let state = crate::random_hex();
        let mut url = url_with(
            &server.authorization_endpoint,
            &[
                ("response_type", "code"),
                ("client_id", &client_id),
                ("redirect_uri", redirect),
                ("code_challenge", &challenge(&verifier)),
                ("code_challenge_method", "S256"),
                ("state", &state),
                ("resource", &server.resource),
            ],
        );
        if !scopes.is_empty() {
            url = url_with(&url, &[("scope", &scopes.join(" "))]);
        }
        let mut flows = self.flows.lock().unwrap();
        flows.retain(|_, f| f.started.elapsed() < FLOW_FOR);
        flows.insert(
            state,
            Flow {
                upstream: name.to_string(),
                verifier,
                client_id,
                token_endpoint: server.token_endpoint,
                resource: server.resource,
                redirect: redirect.to_string(),
                started: Instant::now(),
            },
        );
        Ok(url)
    }

    /// The callback of a login: its `state` names a flow this hub started
    /// (used once, within ten minutes), its `code` is exchanged for tokens.
    /// The upstream it logged in.
    pub fn complete(&self, http: &ureq::Agent, state: &str, code: &str) -> Result<String, String> {
        let flow = {
            let mut flows = self.flows.lock().unwrap();
            flows.retain(|_, f| f.started.elapsed() < FLOW_FOR);
            flows.remove(state)
        }
        .ok_or("no login of this hub waits for that answer (started elsewhere, used, or older than ten minutes)")?;
        let t = token_request(
            http,
            &flow.token_endpoint,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &flow.redirect),
                ("client_id", &flow.client_id),
                ("code_verifier", &flow.verifier),
                ("resource", &flow.resource),
            ],
        )?;
        self.put_secret(
            &flow.upstream,
            &Secret::Oauth {
                access_token: t.access_token,
                refresh_token: t.refresh_token,
                expires_at: t.expires_in.map(|s| now_s() + s),
                client_id: flow.client_id,
                token_endpoint: flow.token_endpoint,
                resource: flow.resource,
            },
        )?;
        Ok(flow.upstream)
    }

    /// The bearer token for a call to `name` now: a static one, or the
    /// login's access token, refreshed when it is about to expire (or when
    /// `stale`, after the upstream refused it). `None`: it needs none, or it
    /// is logged out.
    pub fn bearer(
        &self,
        http: &ureq::Agent,
        name: &str,
        stale: bool,
    ) -> Result<Option<String>, String> {
        let u = self.get(name)?;
        match &u.auth {
            Auth::None => Ok(None),
            Auth::Token { env: Some(e) } => Ok(std::env::var(e).ok()),
            _ => {
                let Some(text) = self.secrets.get(name)? else {
                    return Ok(None);
                };
                match serde_json::from_str::<Secret>(&text).map_err(|e| e.to_string())? {
                    Secret::Token { token } => Ok(Some(token)),
                    Secret::Oauth {
                        access_token,
                        refresh_token,
                        expires_at,
                        client_id,
                        token_endpoint,
                        resource,
                    } => {
                        let due = expires_at.is_some_and(|at| at <= now_s() + 60);
                        let Some(refresh) = refresh_token.filter(|_| due || stale) else {
                            return Ok(Some(access_token));
                        };
                        let t = token_request(
                            http,
                            &token_endpoint,
                            &[
                                ("grant_type", "refresh_token"),
                                ("refresh_token", &refresh),
                                ("client_id", &client_id),
                                ("resource", &resource),
                            ],
                        )?;
                        let access = t.access_token.clone();
                        self.put_secret(
                            name,
                            &Secret::Oauth {
                                access_token: t.access_token,
                                // A server that rotates sends a new one.
                                refresh_token: t.refresh_token.or(Some(refresh)),
                                expires_at: t.expires_in.map(|s| now_s() + s),
                                client_id,
                                token_endpoint,
                                resource,
                            },
                        )?;
                        Ok(Some(access))
                    }
                }
            }
        }
    }
}

/// How long a tool list is taken as current.
const LISTED_FOR: Duration = Duration::from_secs(60);

/// The page of the dashboard where upstreams are logged in.
fn dashboard() -> String {
    format!("http://127.0.0.1:{}/#/upstreams", crate::port())
}

impl Upstreams {
    /// `method` on upstream `u` with its credential; a refused one is
    /// refreshed once (`stale`) before it counts as logged out.
    fn ask(
        &self,
        http: &ureq::Agent,
        u: &Upstream,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        if u.is_command() {
            return self.ask_command(u, method, params);
        }
        for stale in [false, true] {
            let bearer = self.bearer(http, &u.name, stale)?;
            if bearer.is_none() && !matches!(u.auth, Auth::None) {
                return Err(format!(
                    "devstack is not logged in to the MCP upstream `{}`: log in at {}",
                    u.name,
                    dashboard()
                ));
            }
            let mut session = self
                .sessions
                .lock()
                .unwrap()
                .remove(&u.name)
                .unwrap_or_default();
            let r = crate::mcpclient::request(
                http,
                &u.url,
                bearer.as_deref(),
                &mut session,
                method,
                params.clone(),
            );
            self.sessions
                .lock()
                .unwrap()
                .insert(u.name.clone(), session);
            match r {
                Ok(v) => return Ok(v),
                Err(crate::mcpclient::Error::Unauthorized) if !stale => continue,
                Err(crate::mcpclient::Error::Unauthorized) => {
                    return Err(format!(
                        "the MCP upstream `{}` refused devstack's login: log in again at {}",
                        u.name,
                        dashboard()
                    ));
                }
                Err(e) => return Err(format!("the MCP upstream `{}`: {e}", u.name)),
            }
        }
        unreachable!("the loop returns")
    }

    /// `method` on the command of `u`, started when it does not run (card
    /// point 3): with the variables the user set and, for a token the hub
    /// keeps, that token in `token_var`.
    fn ask_command(&self, u: &Upstream, method: &str, params: Value) -> Result<Value, String> {
        let running = self
            .procs
            .lock()
            .unwrap()
            .get(&u.name)
            .filter(|p| p.alive())
            .cloned();
        let p = match running {
            Some(p) => p,
            None => {
                let mut env = u.env.clone();
                if let (Auth::Token { env: None }, Some(var)) = (&u.auth, &u.token_var) {
                    let token = self.bearer(&http(), &u.name, false)?.ok_or_else(|| {
                        format!(
                            "devstack has no token for the MCP upstream `{}`: give it at {}",
                            u.name,
                            dashboard()
                        )
                    })?;
                    env.insert(var.clone(), token);
                }
                if let Auth::Token { env: Some(from) } = &u.auth
                    && let Ok(v) = std::env::var(from)
                {
                    env.insert(from.clone(), v);
                }
                let log = self.logs.join(format!("{}.log", u.name));
                let p = std::sync::Arc::new(
                    crate::stdio::Process::start(&u.command, &env, &log).map_err(|e| {
                        format!(
                            "the MCP upstream `{}`: {e} (its output: {})",
                            u.name,
                            log.display()
                        )
                    })?,
                );
                self.procs.lock().unwrap().insert(u.name.clone(), p.clone());
                p
            }
        };
        let within = if method == "tools/call" {
            Duration::from_secs(600)
        } else {
            Duration::from_secs(60)
        };
        p.request(method, params, within)
            .map_err(|e| format!("the MCP upstream `{}`: {e}", u.name))
    }

    /// The tools `u` offers now, all pages, before its allowlist.
    fn list_tools(&self, http: &ureq::Agent, u: &Upstream) -> Result<Vec<Value>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |c| json!({ "cursor": c }));
            let r = self.ask(http, u, "tools/list", params)?;
            tools.extend(r["tools"].as_array().cloned().unwrap_or_default());
            cursor = r["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    fn seen_tools(&self) -> HashMap<String, Vec<Value>> {
        std::fs::read(&self.seen)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn keep_seen(&self, name: &str, tools: &[Value]) {
        if self.seen.as_os_str().is_empty() {
            return;
        }
        let mut all = self.seen_tools();
        if all.get(name).is_some_and(|t| t == tools) {
            return;
        }
        all.insert(name.to_string(), tools.to_vec());
        if let Ok(text) = serde_json::to_vec(&all) {
            let _ = std::fs::write(&self.seen, text);
        }
    }

    /// What `u` lists now, at most a minute old; `Err` with why it could
    /// not be asked.
    fn current(&self, http: &ureq::Agent, u: &Upstream) -> Result<Vec<Value>, String> {
        if let Some((at, r)) = self.listed.lock().unwrap().get(&u.name)
            && at.elapsed() < LISTED_FOR
        {
            return r.clone();
        }
        let r = self.list_tools(http, u);
        if let Ok(t) = &r {
            self.keep_seen(&u.name, t);
        }
        self.listed
            .lock()
            .unwrap()
            .insert(u.name.clone(), (Instant::now(), r.clone()));
        r
    }

    /// The tools of every upstream, as an agent sees them through
    /// `devstack mcp`: `<prefix>__<tool>`, its allowlist applied. One that
    /// cannot be asked keeps the tools it listed last, said in their
    /// description; one never listed shows a single `<prefix>__login` that
    /// says how to get its tools (card point 5).
    pub fn tools(&self, http: &ureq::Agent) -> Vec<Value> {
        let Ok(all) = self.list() else {
            return Vec::new();
        };
        let seen = self.seen_tools();
        let mut out = Vec::new();
        for u in &all {
            let prefix = u.prefix();
            let (tools, why) = match self.current(http, u) {
                Ok(t) => (t, None),
                Err(e) => (seen.get(&u.name).cloned().unwrap_or_default(), Some(e)),
            };
            if tools.is_empty() {
                if let Some(why) = why {
                    out.push(json!({
                        "name": format!("{prefix}__login"),
                        "description": format!("The tools of the MCP upstream `{}` are not here yet: {why}", u.name),
                        "inputSchema": { "type": "object", "properties": {} },
                    }));
                }
                continue;
            }
            for t in tools {
                let Some(name) = t["name"].as_str() else {
                    continue;
                };
                if !u.allow.is_empty() && !u.allow.iter().any(|a| a == name) {
                    continue;
                }
                let mut t = t.clone();
                t["name"] = json!(format!("{prefix}__{name}"));
                let what = t["description"].as_str().unwrap_or("").to_string();
                t["description"] = json!(match &why {
                    None => format!("[{} via devstack] {what}", u.name),
                    Some(w) => format!("[{} via devstack, not available now: {w}] {what}", u.name),
                });
                out.push(t);
            }
        }
        out
    }

    /// Call `<prefix>__<tool>` with `arguments`: the upstream's
    /// `CallToolResult`, or one with `isError` that says why not.
    pub fn call(&self, http: &ureq::Agent, name: &str, arguments: Value) -> Value {
        let failed = |text: String| json!({ "content": [{ "type": "text", "text": text }], "isError": true });
        let Ok(all) = self.list() else {
            return failed("the hub cannot read its upstreams".into());
        };
        let Some((u, tool)) = all.iter().find_map(|u| {
            name.strip_prefix(&format!("{}__", u.prefix()))
                .map(|t| (u, t))
        }) else {
            return failed(format!("no MCP upstream has the tool `{name}`"));
        };
        if tool == "login"
            && !self
                .seen_tools()
                .get(&u.name)
                .is_some_and(|t| t.iter().any(|x| x["name"] == "login"))
        {
            return failed(format!(
                "log in to the MCP upstream `{}` at {}",
                u.name,
                dashboard()
            ));
        }
        if !u.allow.is_empty() && !u.allow.iter().any(|a| a == tool) {
            return failed(format!(
                "`{tool}` of `{}` is not among the tools allowed in the dashboard",
                u.name
            ));
        }
        match self.ask(
            http,
            u,
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        ) {
            Ok(r) => r,
            Err(e) => failed(e),
        }
    }
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The PKCE S256 challenge of `verifier` (RFC 7636).
fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// `base` with `params` appended to its query, each value encoded.
fn url_with(base: &str, params: &[(&str, &str)]) -> String {
    let mut out = base.to_string();
    for (k, v) in params {
        out.push(if out.contains('?') { '&' } else { '?' });
        out.push_str(k);
        out.push('=');
        out.push_str(&encode(v));
    }
    out
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// An authorization server, as the upstream names it.
#[derive(Debug)]
struct Server {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    /// The upstream as the server knows it (`resource`, RFC 8707).
    resource: String,
}

/// `scheme://host[:port]` and the path of `url`.
fn split(url: &str) -> Result<(String, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("`{url}` is no URL"))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    Ok((
        format!("{scheme}://{host}"),
        path.trim_end_matches('/').to_string(),
    ))
}

fn get_json(http: &ureq::Agent, url: &str) -> Option<serde_json::Value> {
    let mut r = http
        .get(url)
        .header("Accept", "application/json")
        .call()
        .ok()?;
    if r.status() != 200 {
        return None;
    }
    serde_json::from_str(&r.body_mut().read_to_string().ok()?).ok()
}

/// The authorization server of the upstream at `url`: its protected
/// resource metadata (RFC 9728) names it; an upstream without that is its
/// own. Its metadata (RFC 8414, else OpenID discovery).
fn discover(http: &ureq::Agent, url: &str) -> Result<Server, String> {
    let (origin, path) = split(url)?;
    let resource_meta = get_json(
        http,
        &format!("{origin}/.well-known/oauth-protected-resource{path}"),
    )
    .or_else(|| {
        get_json(
            http,
            &format!("{origin}/.well-known/oauth-protected-resource"),
        )
    });
    let resource = resource_meta
        .as_ref()
        .and_then(|m| m["resource"].as_str().map(str::to_string))
        .unwrap_or_else(|| url.trim_end_matches('/').to_string());
    let issuer = resource_meta
        .as_ref()
        .and_then(|m| m["authorization_servers"][0].as_str().map(str::to_string))
        .unwrap_or_else(|| origin.clone());
    let (as_origin, as_path) = split(&issuer)?;
    let meta = [
        format!("{as_origin}/.well-known/oauth-authorization-server{as_path}"),
        format!("{as_origin}/.well-known/openid-configuration{as_path}"),
        format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        ),
    ]
    .iter()
    .find_map(|u| get_json(http, u))
    .ok_or_else(|| format!("{issuer} publishes no OAuth metadata"))?;
    let field = |k: &str| meta[k].as_str().map(str::to_string);
    if meta["code_challenge_methods_supported"]
        .as_array()
        .is_some_and(|m| !m.iter().any(|x| x == "S256"))
    {
        return Err(format!("{issuer} does not take PKCE with S256"));
    }
    Ok(Server {
        authorization_endpoint: field("authorization_endpoint")
            .ok_or_else(|| format!("{issuer} names no authorization endpoint"))?,
        token_endpoint: field("token_endpoint")
            .ok_or_else(|| format!("{issuer} names no token endpoint"))?,
        registration_endpoint: field("registration_endpoint"),
        issuer,
        resource,
    })
}

/// Register this hub as a public client (RFC 7591): its client id.
fn register(http: &ureq::Agent, endpoint: &str, redirect: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "client_name": "devstack hub",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let mut r = http
        .post(endpoint)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(|e| format!("registering at {endpoint}: {e}"))?;
    let text = r.body_mut().read_to_string().unwrap_or_default();
    if !(200..300).contains(&r.status().as_u16()) {
        return Err(format!("registering at {endpoint}: {} {text}", r.status()));
    }
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["client_id"].as_str().map(str::to_string))
        .ok_or_else(|| format!("{endpoint} gave no client id"))
}

#[derive(Debug, Deserialize)]
struct Tokens {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

fn token_request(
    http: &ureq::Agent,
    endpoint: &str,
    form: &[(&str, &str)],
) -> Result<Tokens, String> {
    let body: Vec<String> = form
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect();
    let mut r = http
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(body.join("&"))
        .map_err(|e| format!("{endpoint}: {e}"))?;
    let text = r.body_mut().read_to_string().unwrap_or_default();
    if r.status() != 200 {
        // The server's error, never the request: it held the code.
        let why = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| r.status().to_string());
        return Err(format!("{endpoint} refused: {why}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("{endpoint}: {e}"))
}

/// The agent a tool call goes out with: a tool may work for minutes.
pub fn calls() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// The hub's state directory, where `upstreams.toml` is: for `devstack mcp`
/// without a hub.
pub fn dir() -> PathBuf {
    crate::state_dir()
}

/// The agent the hub talks to upstreams and their servers with: errors as
/// answers, a time limit.
pub fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    #[test]
    fn the_challenge_is_the_unpadded_base64url_of_the_sha256() {
        // Computed apart from this code (Python's hashlib and base64).
        assert_eq!(
            challenge("dBjftJeZ4CVP-mJ92K9-kUVaCKm7mAMd0IoAcnjkJXQ"),
            "ZT_hWS1_xJZTYqGiwg-Oh2kf7rFARJnGs7KTaQOePHk"
        );
    }

    #[test]
    fn an_upstream_is_checked() {
        let u = |name: &str, url: &str| Upstream {
            name: name.into(),
            url: url.into(),
            prefix: None,
            allow: Vec::new(),
            auth: Auth::None,
            ..Default::default()
        };
        assert!(u("tracker", "https://mcp.example.com/mcp").check().is_ok());
        assert!(u("local", "http://127.0.0.1:9000/mcp").check().is_ok());
        assert!(u("Tracker", "https://x").check().is_err());
        assert!(u("tracker", "http://mcp.example.com").check().is_err());
        assert_eq!(u("my-tracker", "https://x").prefix(), "my_tracker");
    }

    #[test]
    fn the_configuration_keeps_no_secret_and_forgets_one_on_a_new_url() {
        let d = tempfile::tempdir().unwrap();
        let ups = Upstreams::open(d.path().to_path_buf());
        let tracker = Upstream {
            name: "tracker".into(),
            url: "https://mcp.example.com/mcp".into(),
            prefix: Some("tr".into()),
            allow: vec!["search".into()],
            auth: Auth::Token { env: None },
            ..Default::default()
        };
        ups.put(tracker.clone()).unwrap();
        ups.set_token("tracker", "secret-token").unwrap();
        let text = std::fs::read_to_string(d.path().join("upstreams.toml")).unwrap();
        assert!(
            text.contains("[[upstream]]") && !text.contains("secret-token"),
            "{text}"
        );
        assert!(ups.views().unwrap()[0].logged_in);
        assert_eq!(
            ups.bearer(&http(), "tracker", false).unwrap().as_deref(),
            Some("secret-token")
        );
        // Another prefix taken is refused.
        let mut other = tracker.clone();
        other.name = "wiki".into();
        assert!(ups.put(other).is_err());
        // A new URL: the token was for the old one.
        let mut moved = tracker.clone();
        moved.url = "https://elsewhere.example.com/mcp".into();
        ups.put(moved).unwrap();
        assert!(!ups.views().unwrap()[0].logged_in);
        assert!(ups.remove("tracker").unwrap());
        assert!(ups.list().unwrap().is_empty());
        // Clean the test's keychain entry, wherever it went.
        let _ = ups.secrets.delete("tracker");
    }

    /// An upstream and its authorization server on one loopback port,
    /// answering metadata, registration and the token endpoint; what each
    /// request sent, by path.
    /// The requests a fake server saw: path and body.
    type Seen = std::sync::Arc<Mutex<Vec<(String, String)>>>;

    fn fake_server() -> (String, Seen) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (b, s) = (base.clone(), seen.clone());
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                s.lock().unwrap().push((path.clone(), body.clone()));
                let (status, json) = match path.as_str() {
                    "/.well-known/oauth-protected-resource/mcp" => (
                        200,
                        serde_json::json!({
                            "resource": format!("{b}/mcp"), "authorization_servers": [format!("{b}/auth")]
                        }),
                    ),
                    "/.well-known/oauth-authorization-server/auth" => (
                        200,
                        serde_json::json!({
                            "issuer": format!("{b}/auth"),
                            "authorization_endpoint": format!("{b}/auth/authorize"),
                            "token_endpoint": format!("{b}/auth/token"),
                            "registration_endpoint": format!("{b}/auth/register"),
                            "code_challenge_methods_supported": ["S256"],
                        }),
                    ),
                    "/auth/register" => (201, serde_json::json!({ "client_id": "hub-client" })),
                    "/auth/token"
                        if body.contains("grant_type=authorization_code")
                            && body.contains("code=the-code") =>
                    {
                        (
                            200,
                            serde_json::json!({ "access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 30 }),
                        )
                    }
                    "/auth/token" if body.contains("grant_type=refresh_token") => (
                        200,
                        serde_json::json!({ "access_token": "access-2", "refresh_token": "refresh-2", "expires_in": 3600 }),
                    ),
                    "/auth/token" => (400, serde_json::json!({ "error": "invalid_grant" })),
                    _ => (404, serde_json::json!({})),
                };
                let text = json.to_string();
                let _ = write!(
                    c,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        (base, seen)
    }

    #[test]
    fn a_login_runs_once_with_pkce_and_refreshes() {
        let (base, seen) = fake_server();
        let d = tempfile::tempdir().unwrap();
        let ups = Upstreams::open(d.path().to_path_buf());
        let name = format!("t{}", std::process::id());
        ups.put(Upstream {
            name: name.clone(),
            url: format!("{base}/mcp"),
            prefix: None,
            allow: Vec::new(),
            auth: Auth::Oauth {
                client_id: None,
                scopes: Vec::new(),
            },
            ..Default::default()
        })
        .unwrap();
        let http = http();
        let redirect = "http://127.0.0.1:4250/oauth/upstream/callback";
        let url = ups.begin(&http, &name, redirect).unwrap();
        assert!(url.starts_with(&format!("{base}/auth/authorize?")), "{url}");
        assert!(
            url.contains("client_id=hub-client") && url.contains("code_challenge_method=S256"),
            "{url}"
        );
        assert!(
            url.contains(&format!("resource={}", encode(&format!("{base}/mcp")))),
            "{url}"
        );
        let state = url
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_string();

        // An answer for no flow of this hub is refused, and changes nothing.
        assert!(ups.complete(&http, "not-a-state", "the-code").is_err());
        assert_eq!(ups.complete(&http, &state, "the-code").unwrap(), name);
        // Once only.
        assert!(ups.complete(&http, &state, "the-code").is_err());
        let sent = seen.lock().unwrap().clone();
        let token = sent
            .iter()
            .find(|(p, b)| p == "/auth/token" && b.contains("authorization_code"))
            .unwrap();
        assert!(
            token.1.contains("code_verifier=") && token.1.contains("client_id=hub-client"),
            "{token:?}"
        );

        // Expiring within a minute: refreshed, and the new refresh token kept.
        assert_eq!(
            ups.bearer(&http, &name, false).unwrap().as_deref(),
            Some("access-2")
        );
        assert_eq!(
            ups.bearer(&http, &name, false).unwrap().as_deref(),
            Some("access-2")
        );
        assert!(ups.views().unwrap()[0].logged_in);
        ups.logout(&name).unwrap();
        assert_eq!(ups.bearer(&http, &name, false).unwrap(), None);
        let _ = ups.remove(&name);
    }

    /// An MCP server over Streamable HTTP that wants `Bearer good`: a
    /// session, `search` and `drop`, `tools/call` answered as an event
    /// stream. What each request named as its method.
    fn fake_mcp() -> (String, Seen) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let seen: Seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let (mut len, mut auth, mut session) = (0, String::new(), String::new());
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    let lower = h.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    } else if lower.starts_with("authorization:") {
                        auth = h[14..].trim().to_string();
                    } else if lower.starts_with("mcp-session-id:") {
                        session = h[15..].trim().to_string();
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let m: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let method = m["method"].as_str().unwrap_or("").to_string();
                s.lock().unwrap().push((method.clone(), session.clone()));
                let reply = |c: &mut std::net::TcpStream,
                             status: u16,
                             ctype: &str,
                             extra: &str,
                             text: &str| {
                    let _ = write!(
                        c,
                        "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                        text.len()
                    );
                };
                if auth != "Bearer good" {
                    reply(&mut c, 401, "application/json", "", "{}");
                    continue;
                }
                let id = m["id"].clone();
                match method.as_str() {
                    "initialize" => {
                        let text = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": "2025-06-18", "capabilities": { "tools": {} } } }).to_string();
                        reply(
                            &mut c,
                            200,
                            "application/json",
                            "Mcp-Session-Id: s-1\r\n",
                            &text,
                        );
                    }
                    "notifications/initialized" => reply(&mut c, 202, "application/json", "", ""),
                    "tools/list" => {
                        let text = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [
                            { "name": "search", "description": "Find issues", "inputSchema": { "type": "object" } },
                            { "name": "drop", "description": "Delete everything", "inputSchema": { "type": "object" } },
                        ] } }).to_string();
                        reply(&mut c, 200, "application/json", "", &text);
                    }
                    "tools/call" => {
                        let q = m["params"]["arguments"]["q"].as_str().unwrap_or("");
                        let answer = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": format!("found {q}") }] } });
                        let text = format!(
                            "event: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}}\n\nevent: message\ndata: {answer}\n\n"
                        );
                        reply(&mut c, 200, "text/event-stream", "", &text);
                    }
                    _ => reply(&mut c, 400, "application/json", "", "{}"),
                }
            }
        });
        (base, seen)
    }

    #[test]
    fn an_upstreams_tools_are_offered_with_its_prefix_and_called_through_the_hub() {
        let (base, seen) = fake_mcp();
        let d = tempfile::tempdir().unwrap();
        let ups = Upstreams::open(d.path().to_path_buf());
        let name = format!("u{}", std::process::id());
        ups.put(Upstream {
            name: name.clone(),
            url: format!("{base}/mcp"),
            prefix: Some("tr".into()),
            allow: vec!["search".into()],
            auth: Auth::Token { env: None },
            ..Default::default()
        })
        .unwrap();
        let http = http();
        // Logged out and never listed: one tool that says how to get them.
        let tools = ups.tools(&http);
        assert_eq!(tools.len(), 1, "{tools:?}");
        assert_eq!(tools[0]["name"], "tr__login");
        assert!(ups.call(&http, "tr__login", json!({}))["isError"] == true);

        ups.set_token(&name, "good").unwrap();
        ups.listed.lock().unwrap().clear();
        let tools = ups.tools(&http);
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["tr__search"], "the allowlist keeps `drop` out");
        assert!(
            tools[0]["description"]
                .as_str()
                .unwrap()
                .contains("via devstack")
        );

        let r = ups.call(&http, "tr__search", json!({ "q": "login bug" }));
        assert_eq!(r["content"][0]["text"], "found login bug", "{r}");
        assert!(
            ups.call(&http, "tr__drop", json!({}))["isError"] == true,
            "not allowed"
        );
        assert!(ups.call(&http, "other__search", json!({}))["isError"] == true);
        // One session for all of it.
        let sent = seen.lock().unwrap().clone();
        assert_eq!(
            sent.iter().filter(|(m, _)| m == "initialize").count(),
            1,
            "{sent:?}"
        );
        assert!(
            sent.iter()
                .filter(|(m, _)| m == "tools/call")
                .all(|(_, s)| s == "s-1"),
            "{sent:?}"
        );

        // A refused token: its tools stay listed, a call says to log in again.
        ups.set_token(&name, "revoked").unwrap();
        ups.sessions.lock().unwrap().clear();
        ups.listed.lock().unwrap().clear();
        let tools = ups.tools(&http);
        assert_eq!(tools.len(), 1);
        assert!(
            tools[0]["description"]
                .as_str()
                .unwrap()
                .contains("not available now"),
            "{tools:?}"
        );
        let r = ups.call(&http, "tr__search", json!({ "q": "x" }));
        assert!(
            r["isError"] == true
                && r["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("log in again"),
            "{r}"
        );
        let _ = ups.remove(&name);
    }

    #[test]
    fn a_command_gets_the_token_the_hub_keeps_in_its_variable_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        let ups = Upstreams::open(d.path().to_path_buf());
        let name = format!("c{}", std::process::id());
        let cmd = Upstream {
            name: name.clone(),
            command: crate::stdio::tests::fake_command(),
            env: std::collections::BTreeMap::from([("DEVSTACK_FAKE_STDIO_MCP".into(), "1".into())]),
            auth: Auth::Token { env: None },
            token_var: Some("THE_TOKEN".into()),
            ..Default::default()
        };
        // What does not fit a command is refused.
        let mut bad = cmd.clone();
        bad.auth = Auth::Oauth {
            client_id: None,
            scopes: Vec::new(),
        };
        assert!(ups.put(bad).is_err());
        let mut bad = cmd.clone();
        bad.token_var = None;
        assert!(ups.put(bad).is_err());
        let mut bad = cmd.clone();
        bad.url = "https://x.example.com/mcp".into();
        assert!(ups.put(bad).is_err());
        ups.put(cmd).unwrap();
        let http = http();
        // No token yet: it is not started, and says how to get one.
        let tools = ups.tools(&http);
        assert_eq!(tools[0]["name"], format!("{name}__login"), "{tools:?}");
        ups.set_token(&name, "tk-1").unwrap();
        ups.listed.lock().unwrap().clear();
        let tools = ups.tools(&http);
        assert_eq!(tools[0]["name"], format!("{name}__env"), "{tools:?}");
        let r = ups.call(
            &http,
            &format!("{name}__env"),
            json!({ "var": "THE_TOKEN" }),
        );
        assert_eq!(r["content"][0]["text"], "tk-1", "{r}");
        // A change of it starts the command again.
        assert_eq!(ups.procs.lock().unwrap().len(), 1);
        ups.remove(&name).unwrap();
        assert!(ups.procs.lock().unwrap().is_empty());
    }
}
