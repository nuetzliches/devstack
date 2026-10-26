//! What a lane's Compose files may ask of a node (ADR-0033, addendum
//! 2026-10-17). The node runs them on its own Docker engine, so a Compose
//! file is root on the node unless the node refuses what reaches beyond a
//! container: privileges, capabilities, devices, the host's namespaces,
//! mounts of the host's files, networks and volumes of other lanes.
//!
//! The model is read by `docker compose config` in a throwaway container
//! that sees only the lane's mirror (read-only, at the same path, without a
//! network): `extends`, `include`, `env_file` and interpolation cannot read
//! a file outside the mirror there, they fail. What the model then names
//! (bind mounts, build contexts, secrets) must resolve beneath the mirror on
//! the node too (a symlink the lane put there does not), and a service may
//! use only the keys this module knows (a key Compose learns later is
//! refused until it is known to be safe).
//!
//! The node then runs exactly the model it checked ([`render`]), written
//! where the lane cannot write: Compose on the node never reads the lane's
//! files again, so the lane cannot change them between the check and the
//! start, and nothing in the model is left for the agent's environment to
//! fill in.

use crate::policy::Network;
use crate::safefs::resolve_beneath;
use devstack_compose::{Group, Model};
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The image `compose config` runs in; `DEVSTACK_NODE_COMPOSE_IMAGE`
/// names another one (it needs `docker compose`).
pub fn config_image() -> String {
    std::env::var("DEVSTACK_NODE_COMPOSE_IMAGE")
        .ok()
        .filter(|i| !i.trim().is_empty())
        .unwrap_or_else(|| "docker:cli".into())
}

/// The arguments of `docker run` that read `g`'s model in a container that
/// sees `src` only. `flags` go to `config`.
pub fn config_args(g: &Group, src: &Path, image: &str, flags: &[&str]) -> Vec<OsString> {
    let mount = format!("{0}:{0}:ro", src.display());
    let mut a: Vec<OsString> = [
        "run",
        "--rm",
        "--network",
        "none",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "256",
        "--memory",
        "512m",
        "-v",
        &mount,
        "-w",
        &g.dir.to_string_lossy(),
    ]
    .iter()
    .map(OsString::from)
    .collect();
    // The group's `DEVSTACK_PORT_*` (ADR-0057) and `DEVSTACK_LANE*`
    // (ADR-0064): Compose in the container interpolates the model with
    // them, nothing else of the agent's.
    for (k, v) in &g.env {
        a.push("-e".into());
        a.push(format!("{k}={v}").into());
    }
    a.extend(["--entrypoint", "docker", image].iter().map(OsString::from));
    a.extend(g.base_args());
    a.extend(
        ["--profile", "*", "config", "--format", "json"]
            .iter()
            .map(OsString::from),
    );
    a.extend(flags.iter().map(OsString::from));
    a
}

/// The paths a group names itself (its Compose files, its env files, the
/// `.env` beside its first file) must resolve beneath `src`.
pub fn check_group(g: &Group, src: &Path) -> Result<(), String> {
    let mut bad = Vec::new();
    for f in g.files.iter().chain(&g.env_files) {
        if let Err(e) = resolve_beneath(src, f) {
            bad.push(e);
        }
    }
    if let Some(dir) = g.files.first().and_then(|f| f.parent()) {
        let dotenv = dir.join(".env");
        if std::fs::symlink_metadata(&dotenv).is_ok()
            && let Err(e) = resolve_beneath(src, &dotenv)
        {
            bad.push(e);
        }
    }
    finish(&g.name, bad)
}

fn finish(group: &str, bad: Vec<String>) -> Result<(), String> {
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "compose group `{group}` asks for what a lane on a node may not have:\n  - {}",
            bad.join("\n  - ")
        ))
    }
}

/// Does `v` name another service of the project (`service:<name>`)?
fn service_ref(v: &str) -> bool {
    v.starts_with("service:")
}

fn empty(v: &Option<String>) -> bool {
    v.as_deref().is_none_or(|s| s.trim().is_empty())
}

/// The services `compose create|up <services>` brings up: those named and
/// what they need (`depends_on`, `links`, a service's network or volumes);
/// every service when none are named.
fn started<'a>(model: &'a Model, services: &[String]) -> BTreeSet<&'a str> {
    if services.is_empty() {
        return model.services.keys().map(String::as_str).collect();
    }
    let mut seen = BTreeSet::new();
    let mut todo: Vec<&str> = services.iter().map(String::as_str).collect();
    while let Some(name) = todo.pop() {
        let Some((name, s)) = model.services.get_key_value(name) else {
            continue;
        };
        if !seen.insert(name.as_str()) {
            continue;
        }
        todo.extend(s.depends_on.keys().map(String::as_str));
        todo.extend(s.links.iter().map(|l| l.split(':').next().unwrap_or(l)));
        todo.extend(
            s.network_mode
                .iter()
                .chain(&s.pid)
                .chain(&s.ipc)
                .filter_map(|v| v.strip_prefix("service:")),
        );
        todo.extend(
            s.volumes_from
                .iter()
                .filter(|v| !v.starts_with("container:"))
                .map(|v| {
                    let v = v.strip_prefix("service:").unwrap_or(v);
                    v.split(':').next().unwrap_or(v)
                }),
        );
    }
    seen
}

/// The top-level networks, volumes, secrets and configs `services` use.
#[derive(Default)]
struct Uses {
    networks: BTreeSet<String>,
    volumes: BTreeSet<String>,
    secrets: BTreeSet<String>,
    configs: BTreeSet<String>,
}

fn uses(model: &Model, services: &BTreeSet<&str>) -> Uses {
    let source = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        v => v.get("source").and_then(|s| s.as_str()).map(String::from),
    };
    let mut u = Uses::default();
    for name in services {
        let s = &model.services[*name];
        if s.networks.is_empty() && s.network_mode.is_none() {
            u.networks.insert("default".into());
        }
        u.networks.extend(s.networks.keys().cloned());
        u.volumes.extend(
            s.volumes
                .iter()
                .filter(|m| m.kind == "volume")
                .filter_map(|m| m.source.clone()),
        );
        u.secrets.extend(s.secrets.iter().filter_map(source));
        // A build reads its secrets on the node too.
        if let Some(b) = &s.build {
            u.secrets.extend(b.secrets.iter().filter_map(source));
        }
        u.configs.extend(s.configs.iter().filter_map(source));
    }
    u
}

/// Check a group's model for a lane on a node under `network`: every
/// problem, in words. `services` are those the group names
/// (`[compose.<group>] services`): only they and what they need run, so only
/// they are checked, and only the networks, volumes, secrets and configs
/// they use. A Compose file may hold more (a production stack's other
/// services) as long as the lane does not start them. With none named all
/// run, and everything is checked.
pub fn check_model(
    group: &str,
    model: &Model,
    src: &Path,
    network: Network,
    own_networks: &[String],
    services: &[String],
) -> Result<(), String> {
    finish(
        group,
        model_problems(model, src, network, own_networks, services),
    )
}

/// [`check_model`], and the keys of `raw`, the model as `config` wrote it:
/// what the lane's services may use at all ([`SERVICE_KEYS`]).
pub fn check(
    group: &str,
    raw: &Value,
    model: &Model,
    src: &Path,
    network: Network,
    own_networks: &[String],
    services: &[String],
) -> Result<(), String> {
    let mut bad = model_problems(model, src, network, own_networks, services);
    bad.extend(key_problems(raw, &started(model, services)));
    finish(group, bad)
}

fn model_problems(
    model: &Model,
    src: &Path,
    network: Network,
    own_networks: &[String],
    services: &[String],
) -> Vec<String> {
    let mut bad = Vec::new();
    let beneath = |what: &str, p: &str, bad: &mut Vec<String>| {
        if let Err(e) = resolve_beneath(src, Path::new(p)) {
            bad.push(format!("{what}: {e}"));
        }
    };
    let run = started(model, services);
    let used = (!services.is_empty()).then(|| uses(model, &run));
    let checked = |set: fn(&Uses) -> &BTreeSet<String>, key: &str| {
        used.as_ref().is_none_or(|u| set(u).contains(key))
    };
    for (name, s) in model
        .services
        .iter()
        .filter(|(n, _)| run.contains(n.as_str()))
    {
        let mut no = |what: String| bad.push(format!("service `{name}`: {what}"));
        if s.privileged {
            no("`privileged`".into());
        }
        if !s.cap_add.is_empty() {
            no(format!("`cap_add` ({})", s.cap_add.join(", ")));
        }
        if !s.devices.is_empty() || !s.device_cgroup_rules.is_empty() {
            no("`devices`".into());
        }
        for (key, v, ok) in [
            ("pid", &s.pid, &[][..]),
            ("ipc", &s.ipc, &["private", "shareable", "none"][..]),
            ("uts", &s.uts, &[][..]),
            ("cgroup", &s.cgroup, &["private"][..]),
            ("network_mode", &s.network_mode, &["none"][..]),
        ] {
            if let Some(v) = v.as_deref().filter(|v| !v.trim().is_empty())
                && !service_ref(v)
                && !ok.contains(&v)
            {
                no(format!("`{key}: {v}`"));
            }
        }
        if !empty(&s.userns_mode) {
            no("`userns_mode`".into());
        }
        if !empty(&s.cgroup_parent) {
            no("`cgroup_parent`".into());
        }
        if let Some(r) = s
            .runtime
            .as_deref()
            .filter(|r| !matches!(*r, "" | "runc" | "nvidia"))
        {
            no(format!("`runtime: {r}`"));
        }
        let opts: Vec<&String> = s
            .security_opt
            .iter()
            .filter(|o| !matches!(o.as_str(), "no-new-privileges" | "no-new-privileges:true"))
            .collect();
        if !opts.is_empty() {
            no(format!(
                "`security_opt` ({})",
                opts.iter()
                    .map(|o| o.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if s.sysctls.as_ref().is_some_and(|v| match v {
            serde_json::Value::Object(m) => !m.is_empty(),
            serde_json::Value::Array(a) => !a.is_empty(),
            serde_json::Value::Null => false,
            _ => true,
        }) {
            no("`sysctls`".into());
        }
        for v in &s.volumes_from {
            if v.starts_with("container:") {
                no(format!("`volumes_from: {v}`"));
            }
        }
        let mut paths: Vec<(String, String)> = Vec::new();
        for m in &s.volumes {
            match m.kind.as_str() {
                "bind" => match &m.source {
                    Some(p) => paths.push(("a bind mount".into(), p.clone())),
                    None => no("a bind mount without a source".into()),
                },
                "volume" | "tmpfs" | "image" | "" => {}
                k => no(format!("a mount of type `{k}`")),
            }
        }
        for e in &s.env_file {
            let p = match e {
                serde_json::Value::String(p) => Some(p.clone()),
                serde_json::Value::Object(o) => {
                    o.get("path").and_then(|p| p.as_str()).map(String::from)
                }
                _ => None,
            };
            if let Some(p) = p {
                paths.push(("an env file".into(), p));
            }
        }
        if let Some(b) = &s.build {
            let remote = |c: &str| c.contains("://") || c.starts_with("git@");
            if let Some(c) = b.context.as_deref().filter(|c| !remote(c)) {
                paths.push(("a build context".into(), c.to_string()));
                if let Some(df) = &b.dockerfile {
                    paths.push((
                        "a Dockerfile".into(),
                        Path::new(c).join(df).to_string_lossy().into_owned(),
                    ));
                }
            }
            for (k, c) in &b.additional_contexts {
                if c.starts_with("oci-layout://") {
                    no(format!("build context `{k}`: an OCI layout from the node"));
                } else if !c.contains("://")
                    && !c.starts_with("service:")
                    && !c.starts_with("target:")
                {
                    paths.push((format!("build context `{k}`"), c.clone()));
                }
            }
            if let Some(n) = b
                .network
                .as_deref()
                .filter(|n| !matches!(*n, "" | "default" | "none"))
            {
                no(format!("`build.network: {n}`"));
            }
            if b.privileged || !b.entitlements.is_empty() {
                no("a privileged build".into());
            }
            if b.ssh
                .as_ref()
                .is_some_and(|v| !v.is_null() && v != &serde_json::json!([]))
            {
                no("`build.ssh` (the node's SSH agent)".into());
            }
            // The node names what it builds (ADR-0033 point 9): a tag of
            // the lane's own could be an image another lane pulls by name.
            if !b.tags.is_empty() {
                no(format!(
                    "`build.tags` ({}): the node names the images it builds",
                    b.tags.join(", ")
                ));
            }
            if !b.cache_to.is_empty() {
                no("`build.cache_to` (it writes to the node or a registry as the node)".into());
            }
            for c in &b.cache_from {
                if c.split(',').any(|kv| kv.trim() == "type=local") {
                    no(format!("`build.cache_from: {c}` (a path on the node)"));
                }
            }
        }
        for (what, p) in paths {
            beneath(&format!("service `{name}`, {what}"), &p, &mut bad);
        }
    }
    for (key, v) in &model.volumes {
        let Some(v) = v.as_ref().filter(|_| checked(|u| &u.volumes, key)) else {
            continue;
        };
        if v.external {
            bad.push(format!(
                "volume `{key}`: an external volume (it may be another lane's)"
            ));
        }
        if v.driver.as_deref().is_some_and(|d| d != "local") {
            bad.push(format!(
                "volume `{key}`: the driver `{}`",
                v.driver.as_deref().unwrap_or("")
            ));
        }
        if !v.driver_opts.is_empty() {
            bad.push(format!(
                "volume `{key}`: `driver_opts` (a volume can bind any path of the node)"
            ));
        }
    }
    for (key, n) in &model.networks {
        let Some(n) = n.as_ref().filter(|_| checked(|u| &u.networks, key)) else {
            continue;
        };
        let name = n.name.as_deref().unwrap_or(key);
        if n.external && !own_networks.iter().any(|o| o == name) {
            bad.push(format!(
                "network `{key}`: an external network the stack file does not own (`[compose.<group>] networks`)"
            ));
        }
        if n.driver.as_deref().is_some_and(|d| d != "bridge") {
            bad.push(format!(
                "network `{key}`: the driver `{}` (only `bridge`: others reach the node's network directly)",
                n.driver.as_deref().unwrap_or("")
            ));
        }
        if n.driver_opts
            .keys()
            .any(|k| k != "com.docker.network.driver.mtu")
        {
            bad.push(format!("network `{key}`: `driver_opts` other than the MTU"));
        }
        if network != Network::Lan {
            let addresses = n.ipam.as_ref().is_some_and(|i| {
                i.get("config")
                    .and_then(|c| c.as_array())
                    .is_some_and(|c| !c.is_empty())
            });
            if addresses {
                bad.push(format!(
                    "network `{key}`: its own addresses (`ipam`); on this node Docker chooses them"
                ));
            }
            if n.enable_ipv6 == Some(true) {
                bad.push(format!(
                    "network `{key}`: IPv6 (this node's rules are IPv4's)"
                ));
            }
        }
    }
    let secrets: fn(&Uses) -> &BTreeSet<String> = |u| &u.secrets;
    let configs: fn(&Uses) -> &BTreeSet<String> = |u| &u.configs;
    for (kind, items, set) in [
        ("secret", &model.secrets, secrets),
        ("config", &model.configs, configs),
    ] {
        for (key, f) in items {
            let Some(f) = f.as_ref().filter(|_| checked(set, key)) else {
                continue;
            };
            if f.external {
                bad.push(format!("{kind} `{key}`: an external one"));
            }
            if let Some(p) = &f.file {
                beneath(&format!("{kind} `{key}`"), p, &mut bad);
            }
            if let Some(v) = &f.environment {
                bad.push(format!(
                    "{kind} `{key}`: from the node's environment (`environment: {v}`)"
                ));
            }
        }
    }
    bad
}

/// The top-level keys a lane's model may have.
const TOP_KEYS: &[&str] = &[
    "name", "services", "networks", "volumes", "secrets", "configs",
];

/// The keys a service may use on a node: those that stay inside its
/// container, and those [`check_model`] checks. Any other is refused, with
/// its name, until it is known to be safe: Compose keeps adding keys that
/// reach the node (`use_api_socket` mounts its Docker socket, `provider`
/// runs one of its programs).
const SERVICE_KEYS: &[&str] = &[
    "annotations",
    "attach",
    "blkio_config",
    "build",
    "cap_add",
    "cap_drop",
    "cgroup",
    "cgroup_parent",
    "command",
    "configs",
    "container_name",
    "cpu_count",
    "cpu_percent",
    "cpu_period",
    "cpu_quota",
    "cpu_shares",
    "cpus",
    "cpuset",
    "depends_on",
    "deploy",
    "develop",
    "device_cgroup_rules",
    "devices",
    "dns",
    "dns_opt",
    "dns_search",
    "domainname",
    "entrypoint",
    "env_file",
    "environment",
    "expose",
    "extra_hosts",
    "gpus",
    "group_add",
    "healthcheck",
    "hostname",
    "image",
    "init",
    "ipc",
    "labels",
    "label_file",
    "links",
    "logging",
    "mac_address",
    "mem_limit",
    "mem_reservation",
    "mem_swappiness",
    "memswap_limit",
    "network_mode",
    "networks",
    "oom_kill_disable",
    "oom_score_adj",
    "pid",
    "pids_limit",
    "platform",
    "ports",
    "post_start",
    "pre_stop",
    "privileged",
    "profiles",
    "pull_policy",
    "pull_refresh_after",
    "read_only",
    "restart",
    "runtime",
    "scale",
    "secrets",
    "security_opt",
    "shm_size",
    "stdin_open",
    "stop_grace_period",
    "stop_signal",
    "storage_opt",
    "sysctls",
    "tmpfs",
    "tty",
    "ulimits",
    "user",
    "userns_mode",
    "uts",
    "volumes",
    "volumes_from",
    "working_dir",
];

/// The keys of a service's `build` a node knows.
const BUILD_KEYS: &[&str] = &[
    "context",
    "dockerfile",
    "dockerfile_inline",
    "args",
    "ssh",
    "cache_from",
    "cache_to",
    "additional_contexts",
    "entitlements",
    "extra_hosts",
    "labels",
    "network",
    "no_cache",
    "platforms",
    "privileged",
    "pull",
    "secrets",
    "shm_size",
    "tags",
    "target",
    "ulimits",
    "provenance",
    "sbom",
];

fn known(keys: &[&str], k: &str) -> bool {
    keys.contains(&k) || k.starts_with("x-")
}

/// What [`check_model`]'s typed model cannot see: keys no node knows, and
/// values of known keys that reach the node.
fn key_problems(raw: &Value, run: &BTreeSet<&str>) -> Vec<String> {
    let mut bad = Vec::new();
    if let Some(top) = raw.as_object() {
        for k in top.keys().filter(|k| !known(TOP_KEYS, k)) {
            bad.push(format!(
                "`{k}` at the top level (not known to be safe on a node)"
            ));
        }
    }
    let Some(services) = raw.get("services").and_then(Value::as_object) else {
        return bad;
    };
    for (name, s) in services.iter().filter(|(n, _)| run.contains(n.as_str())) {
        let Some(s) = s.as_object() else { continue };
        let mut no = |what: String| bad.push(format!("service `{name}`: {what}"));
        for (k, v) in s {
            match k.as_str() {
                "provider" => no("`provider` (Compose runs a program of the node for it)".into()),
                "use_api_socket" if v.as_bool() != Some(false) => {
                    no("`use_api_socket` (the node's Docker socket)".into())
                }
                "use_api_socket" => {}
                k if known(SERVICE_KEYS, k) => {}
                k => no(format!("`{k}` (not known to be safe on a node)")),
            }
        }
        if s.get("oom_kill_disable").and_then(Value::as_bool) == Some(true) {
            no("`oom_kill_disable` (the node's kernel could not end it)".into());
        }
        if s.get("oom_score_adj")
            .and_then(Value::as_i64)
            .is_some_and(|v| v < 0)
        {
            no("a negative `oom_score_adj`".into());
        }
        if let Some(d) = s
            .get("logging")
            .and_then(|l| l.get("driver"))
            .and_then(Value::as_str)
            .filter(|d| !matches!(*d, "" | "json-file" | "local" | "none"))
        {
            no(format!(
                "`logging.driver: {d}` (the node's engine would send the logs)"
            ));
        }
        for hook in ["post_start", "pre_stop"] {
            let privileged = s.get(hook).and_then(Value::as_array).is_some_and(|hs| {
                hs.iter()
                    .any(|h| h.get("privileged").and_then(Value::as_bool) == Some(true))
            });
            if privileged {
                no(format!("a privileged `{hook}` hook"));
            }
        }
        let devices = s
            .get("deploy")
            .and_then(|d| d.pointer("/resources/reservations/devices"))
            .and_then(Value::as_array);
        for d in devices.into_iter().flatten() {
            let driver = d.get("driver").and_then(Value::as_str).unwrap_or("");
            let gpu = d
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|c| c.iter().any(|c| c.as_str() == Some("gpu")));
            if !(driver == "nvidia" || driver.is_empty() && gpu) {
                no(format!(
                    "`deploy.resources.reservations.devices` with the driver `{driver}` (only GPUs, `nvidia`)"
                ));
            }
        }
        if let Some(b) = s.get("build").and_then(Value::as_object) {
            for k in b.keys().filter(|k| !known(BUILD_KEYS, k)) {
                no(format!("`build.{k}` (not known to be safe on a node)"));
            }
        }
    }
    bad
}

/// Unset values in `raw`, a model as `config` wrote it, go: a service's
/// `environment: [NAME]` and a build's `args: [NAME]` without a value
/// would take theirs from the environment of the Compose that runs them,
/// the node agent's (`config` leaves them for `create` to fill in).
pub fn strip_unset(raw: &mut Value) {
    let Some(services) = raw.get_mut("services").and_then(Value::as_object_mut) else {
        return;
    };
    for s in services.values_mut() {
        if let Some(env) = s.get_mut("environment").and_then(Value::as_object_mut) {
            env.retain(|_, v| !v.is_null());
        }
        if let Some(args) = s
            .get_mut("build")
            .and_then(|b| b.get_mut("args"))
            .and_then(Value::as_object_mut)
        {
            args.retain(|_, v| !v.is_null());
        }
    }
}

/// The most bytes of a secret's or config's file the agent copies.
const MAX_FILE: u64 = 4 << 20;

/// The model a node runs for a group, from `raw` as `config` wrote it and
/// [`check`] passed it (`model` is the same, typed), as a Compose file of
/// its own the lane cannot write:
///
/// - only the services the lane starts ([`check_model`] checked no other);
/// - each bind mount from the mirror becomes a mount of one volume bound to
///   the mirror, with the mount's path as its `subpath`: Docker resolves a
///   subpath beneath its volume and refuses a link out of it, at every
///   start of the container (a bind mount follows a link the lane's code
///   puts there after the check, at the next restart);
/// - each secret's and config's file is copied into `files`, so a link put
///   in its place later reads nothing of the node's.
pub fn render(
    mut raw: Value,
    model: &Model,
    src: &Path,
    services: &[String],
    files: &Path,
) -> Result<String, String> {
    let root = std::fs::canonicalize(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let run = started(model, services);
    let used = uses(model, &run);
    let beneath = |what: &str, p: &str| -> Result<(PathBuf, PathBuf), String> {
        let full = resolve_beneath(&root, Path::new(p)).map_err(|e| format!("{what}: {e}"))?;
        let rel = full
            .strip_prefix(&root)
            .map_err(|_| format!("{what}: `{p}` is outside the lane"))?
            .to_path_buf();
        Ok((full, rel))
    };
    let mirror = format!(
        "devstack-mirror-{}",
        &blake3::hash(root.to_string_lossy().as_bytes()).to_hex()[..12]
    );
    let mut bound = false;
    if let Some(svcs) = raw.get_mut("services").and_then(Value::as_object_mut) {
        svcs.retain(|n, _| run.contains(n.as_str()));
        for (name, s) in svcs.iter_mut() {
            let Some(mounts) = s.get_mut("volumes").and_then(Value::as_array_mut) else {
                continue;
            };
            for m in mounts.iter_mut() {
                if m.get("type").and_then(Value::as_str) != Some("bind") {
                    continue;
                }
                let what = format!("service `{name}`, a bind mount");
                let source = m.get("source").and_then(Value::as_str).unwrap_or("");
                let (full, rel) = beneath(&what, source)?;
                if !full.exists() && !rel.as_os_str().is_empty() {
                    // Docker makes a missing bind source; a subpath it does not.
                    crate::safefs::make_dirs_beneath(&root, &rel)
                        .map_err(|e| format!("{what}: {e}"))?;
                }
                let sub: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let mut volume = serde_json::json!({ "nocopy": true });
                if !sub.is_empty() {
                    volume["subpath"] = Value::String(sub.join("/"));
                }
                let mut out = serde_json::Map::new();
                out.insert("type".into(), "volume".into());
                out.insert("source".into(), mirror.clone().into());
                for k in ["target", "read_only", "consistency"] {
                    if let Some(v) = m.get(k) {
                        out.insert(k.into(), v.clone());
                    }
                }
                out.insert("volume".into(), volume);
                *m = Value::Object(out);
                bound = true;
            }
        }
    }
    if bound {
        let vols = raw
            .as_object_mut()
            .ok_or("the model is no object")?
            .entry("volumes")
            .or_insert_with(|| Value::Object(Default::default()));
        if vols.is_null() {
            *vols = Value::Object(Default::default());
        }
        vols.as_object_mut().ok_or("`volumes` is no map")?.insert(
            mirror,
            serde_json::json!({
                "driver": "local",
                "driver_opts": { "type": "none", "o": "bind", "device": root.to_string_lossy() },
            }),
        );
    }
    let _ = std::fs::remove_dir_all(files);
    for (kind, set) in [("secrets", &used.secrets), ("configs", &used.configs)] {
        let Some(items) = raw.get_mut(kind).and_then(Value::as_object_mut) else {
            continue;
        };
        items.retain(|k, _| set.contains(k));
        for (key, item) in items.iter_mut() {
            let Some(p) = item.get("file").and_then(Value::as_str) else {
                continue;
            };
            let what = format!("{kind} `{key}`");
            let (_, rel) = beneath(&what, p)?;
            let text = crate::safefs::read_beneath(&root, &rel, MAX_FILE)
                .map_err(|e| format!("{what}: {e}"))?;
            std::fs::create_dir_all(files).map_err(|e| format!("{}: {e}", files.display()))?;
            let copy = files.join(format!(
                "{kind}-{}",
                &blake3::hash(key.as_bytes()).to_hex()[..16]
            ));
            std::fs::write(&copy, text).map_err(|e| format!("{}: {e}", copy.display()))?;
            item["file"] = Value::String(copy.to_string_lossy().into_owned());
        }
    }
    serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(json: &str) -> Model {
        devstack_compose::parse_config(json).unwrap()
    }

    #[test]
    fn what_reaches_beyond_a_container_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(src.join("db/init")).unwrap();
        let s = src.display().to_string().replace('\\', "\\\\");
        let fine = model(&format!(
            r#"{{"services":{{"db":{{"image":"postgres:17",
                "volumes":[{{"type":"bind","source":"{s}/db/init","target":"/docker-entrypoint-initdb.d"}},
                           {{"type":"volume","source":"data","target":"/var/lib/postgresql/data"}},
                           {{"type":"tmpfs","target":"/tmp"}}],
                "security_opt":["no-new-privileges:true"],"ipc":"shareable",
                "build":{{"context":"{s}/db","dockerfile":"Dockerfile"}}}},
              "app":{{"network_mode":"service:db","pid":"service:db"}}}},
              "volumes":{{"data":{{"name":"shop-ab_data"}}}},
              "networks":{{"default":{{"name":"shop-ab_default"}},"edge":{{"name":"edge","external":true}}}}}}"#
        ));
        assert!(
            check_model("app", &fine, &src, Network::Internet, &["edge".into()], &[]).is_ok(),
            "{:?}",
            check_model("app", &fine, &src, Network::Internet, &["edge".into()], &[])
        );

        let root = model(&format!(
            r#"{{"services":{{"evil":{{"image":"alpine","privileged":true,"cap_add":["SYS_ADMIN"],
                "devices":[{{"source":"/dev/sda","target":"/dev/sda"}}],"pid":"host","ipc":"host",
                "network_mode":"host","userns_mode":"host","cgroup_parent":"/","uts":"host",
                "security_opt":["apparmor=unconfined"],"sysctls":{{"net.ipv4.ip_forward":"1"}},
                "volumes":[{{"type":"bind","source":"/","target":"/host"}},
                           {{"type":"bind","source":"/var/run/docker.sock","target":"/var/run/docker.sock"}},
                           {{"type":"bind","source":"{s}/../other/src","target":"/x"}}],
                "volumes_from":["container:other"],
                "env_file":[{{"path":"/etc/environment"}}],
                "build":{{"context":"/","network":"host","ssh":["default"]}}}}}},
              "volumes":{{"theirs":{{"name":"shop-cd_data","external":true}},
                         "bind":{{"driver_opts":{{"type":"none","o":"bind","device":"/"}}}}}},
              "networks":{{"lan":{{"driver":"macvlan"}},"theirs":{{"name":"shop-cd_default","external":true}},
                          "wide":{{"ipam":{{"config":[{{"subnet":"10.0.0.0/8"}}]}},"enable_ipv6":true}}}},
              "secrets":{{"key":{{"file":"/etc/shadow"}}}}}}"#
        ));
        let e = check_model("app", &root, &src, Network::Internet, &[], &[]).unwrap_err();
        for what in [
            "`privileged`",
            "`cap_add` (SYS_ADMIN)",
            "`devices`",
            "`pid: host`",
            "`ipc: host`",
            "`network_mode: host`",
            "`uts: host`",
            "`userns_mode`",
            "`cgroup_parent`",
            "`security_opt` (apparmor=unconfined)",
            "`sysctls`",
            "`volumes_from: container:other`",
            "`build.network: host`",
            "`build.ssh`",
            "volume `theirs`: an external volume",
            "volume `bind`: `driver_opts`",
            "network `lan`: the driver `macvlan`",
            "network `theirs`: an external network",
            "network `wide`: its own addresses",
            "network `wide`: IPv6",
            "secret `key`",
            "an env file",
            "a build context",
        ] {
            assert!(e.contains(what), "{what} missing in:\n{e}");
        }
        // Three bind mounts, all outside.
        assert_eq!(e.matches("a bind mount").count(), 3, "{e}");
        // On a `lan` node, addresses and IPv6 of the lane's own are its own.
        let e = check_model("app", &root, &src, Network::Lan, &[], &[]).unwrap_err();
        assert!(
            !e.contains("its own addresses") && !e.contains("IPv6"),
            "{e}"
        );
    }

    #[test]
    fn only_what_the_group_starts_is_checked() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        // A production Compose file: the lane starts `api` (and `db`, which
        // it needs), never `ops` or `edge`.
        let m = model(
            r#"{"services":{
                "api":{"image":"api","depends_on":{"db":{"condition":"service_started"}},
                       "networks":{"default":null}},
                "db":{"image":"postgres","volumes":[{"type":"volume","source":"data","target":"/d"}],
                      "secrets":[{"source":"pw","target":"/run/secrets/pw"}]},
                "ops":{"image":"ops","volumes":[{"type":"bind","source":"/var/run/docker.sock","target":"/s"}],
                       "networks":{"es":null}},
                "edge":{"image":"caddy","privileged":true,"volumes":[{"type":"volume","source":"theirs","target":"/t"}],
                        "secrets":[{"source":"key","target":"/k"}]}},
              "networks":{"default":{"name":"p_default"},"es":{"name":"shared-es","external":true}},
              "volumes":{"data":{"name":"p_data"},"theirs":{"name":"x","external":true}},
              "secrets":{"pw":{"file":"/etc/shadow"},"key":{"file":"/etc/ssh/key"}}}"#,
        );
        let api = ["api".to_string()];
        let e = check_model("app", &m, &src, Network::Internet, &[], &api).unwrap_err();
        // `db` runs with `api`: its secret is checked.
        assert!(e.contains("secret `pw`"), "{e}");
        for not in [
            "`ops`",
            "`edge`",
            "network `es`",
            "volume `theirs`",
            "secret `key`",
        ] {
            assert!(!e.contains(not), "{not} in:\n{e}");
        }
        // A named service on an external network: refused.
        let e = check_model("app", &m, &src, Network::Internet, &[], &["ops".into()]).unwrap_err();
        assert!(
            e.contains("network `es`") && e.contains("service `ops`"),
            "{e}"
        );
        // None named: everything runs, everything is checked.
        let e = check_model("app", &m, &src, Network::Internet, &[], &[]).unwrap_err();
        for what in [
            "service `edge`",
            "service `ops`",
            "network `es`",
            "volume `theirs`",
            "secret `key`",
        ] {
            assert!(e.contains(what), "{what} missing in:\n{e}");
        }
    }

    #[test]
    fn keys_that_reach_the_node_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let ctx = src.to_string_lossy().into_owned();
        let raw = serde_json::json!({
            "name": "p",
            "models": {"llm": {"model": "ai/x"}},
            "services": {
                "api": {
                    "image": "api", "use_api_socket": true, "oom_kill_disable": true,
                    "oom_score_adj": -500, "frobnicate": 1, "x-ours": {},
                    "logging": {"driver": "syslog", "options": {"syslog-address": "tcp://10.0.0.1:514"}},
                    "post_start": [{"command": "id", "privileged": true}],
                    "deploy": {"resources": {"reservations": {"devices": [
                        {"driver": "cdi", "device_ids": ["vendor.com/dev=all"], "capabilities": ["x"]},
                        {"driver": "nvidia", "count": 1, "capabilities": ["gpu"]},
                        {"capabilities": ["gpu"]}
                    ]}}},
                    "build": {"context": ctx, "tags": ["postgres:17"], "cache_to": ["type=local,dest=/x"],
                              "cache_from": ["type=local,src=/var/lib", "registry.example/app:cache"],
                              "secrets": ["tok"], "frob": true}
                },
                "p": {"provider": {"type": "/bin/sh", "options": {"c": ["id"]}}},
                "fine": {"image": "postgres:17", "use_api_socket": false, "gpus": [{"count": -1}],
                         "logging": {"driver": "json-file", "options": {"max-size": "10m"}},
                         "oom_score_adj": 100}
            },
            "secrets": {"tok": {"environment": "GITHUB_TOKEN"}},
            "configs": {"c": {"content": "x"}}
        });
        let model: Model = serde_json::from_value(raw.clone()).unwrap();
        let e = check("app", &raw, &model, &src, Network::Internet, &[], &[]).unwrap_err();
        for what in [
            "`models` at the top level",
            "service `api`: `use_api_socket`",
            "service `api`: `oom_kill_disable`",
            "service `api`: a negative `oom_score_adj`",
            "service `api`: `frobnicate`",
            "service `api`: `logging.driver: syslog`",
            "service `api`: a privileged `post_start` hook",
            "with the driver `cdi`",
            "service `api`: `build.tags` (postgres:17)",
            "service `api`: `build.cache_to`",
            "`build.cache_from: type=local,src=/var/lib`",
            "service `api`: `build.frob`",
            "service `p`: `provider`",
            // A build's secret counts as used, and is checked.
            "secret `tok`: from the node's environment (`environment: GITHUB_TOKEN`)",
        ] {
            assert!(e.contains(what), "{what} missing in:\n{e}");
        }
        for not in [
            "x-ours",
            "service `fine`",
            "registry.example",
            "the driver `nvidia`",
            "the driver ``",
            "config `c`",
        ] {
            assert!(!e.contains(not), "{not} in:\n{e}");
        }
    }

    #[test]
    fn unset_values_do_not_reach_the_nodes_environment() {
        let mut raw = serde_json::json!({"services": {"api": {
            "environment": {"MODE": "dev", "GITHUB_TOKEN": null},
            "build": {"args": {"V": "1", "NPM_TOKEN": null}}
        }}});
        strip_unset(&mut raw);
        assert_eq!(
            raw["services"]["api"]["environment"],
            serde_json::json!({"MODE": "dev"})
        );
        assert_eq!(
            raw["services"]["api"]["build"]["args"],
            serde_json::json!({"V": "1"})
        );
    }

    #[test]
    fn the_node_runs_what_was_checked() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(src.join("db/init")).unwrap();
        std::fs::write(src.join("db/pw.txt"), "s3cret").unwrap();
        let files = d.path().join("compose/files/app");
        let s = |rel: &str| src.join(rel).to_string_lossy().into_owned();
        let raw = serde_json::json!({
            "name": "p",
            "services": {
                "api": {"image": "api", "depends_on": {"db": {"condition": "service_started"}},
                        "volumes": [{"type": "bind", "source": s(""), "target": "/work", "bind": {"create_host_path": true}}]},
                "db": {"image": "postgres:17",
                       "volumes": [
                           {"type": "bind", "source": s("db/init"), "target": "/docker-entrypoint-initdb.d", "read_only": true},
                           {"type": "bind", "source": s("db/new/dir"), "target": "/new"},
                           {"type": "volume", "source": "data", "target": "/var/lib/postgresql/data"}],
                       "secrets": [{"source": "pw", "target": "/run/secrets/pw"}]},
                "ops": {"image": "ops", "volumes": [{"type": "bind", "source": "/var/run/docker.sock", "target": "/s"}],
                        "secrets": [{"source": "key", "target": "/k"}]}
            },
            "volumes": {"data": {"name": "p_data"}},
            "secrets": {"pw": {"name": "p_pw", "file": s("db/pw.txt")}, "key": {"file": "/etc/ssh/key"}}
        });
        let model: Model = serde_json::from_value(raw.clone()).unwrap();
        let text = render(raw, &model, &src, &["api".into()], &files).unwrap();
        let out: Value = serde_json::from_str(&text).unwrap();
        // Only what the lane starts; what it does not was never checked.
        let svcs = out["services"].as_object().unwrap();
        assert_eq!(svcs.keys().collect::<Vec<_>>(), ["api", "db"]);
        assert!(out["secrets"].get("key").is_none(), "{text}");
        // Bind mounts are subpaths of one volume bound to the mirror.
        let key = out["volumes"]
            .as_object()
            .unwrap()
            .keys()
            .find(|k| k.starts_with("devstack-mirror-"))
            .unwrap()
            .clone();
        let root = std::fs::canonicalize(&src).unwrap();
        assert_eq!(
            out["volumes"][&key]["driver_opts"]["device"],
            serde_json::json!(root.to_string_lossy())
        );
        assert_eq!(
            out["services"]["api"]["volumes"][0],
            serde_json::json!({"type": "volume", "source": key, "target": "/work", "volume": {"nocopy": true}})
        );
        assert_eq!(
            out["services"]["db"]["volumes"][0],
            serde_json::json!({"type": "volume", "source": key, "target": "/docker-entrypoint-initdb.d",
                               "read_only": true, "volume": {"nocopy": true, "subpath": "db/init"}})
        );
        // A missing source is made, as Docker would.
        assert_eq!(
            out["services"]["db"]["volumes"][1]["volume"]["subpath"],
            "db/new/dir"
        );
        assert!(src.join("db/new/dir").is_dir());
        assert_eq!(out["services"]["db"]["volumes"][2]["source"], "data");
        // A secret's file is a copy the lane cannot change.
        let pw = out["secrets"]["pw"]["file"].as_str().unwrap();
        assert!(Path::new(pw).starts_with(&files), "{pw}");
        assert_eq!(std::fs::read_to_string(pw).unwrap(), "s3cret");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_put_in_the_mirror_is_not_rendered() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::os::unix::fs::symlink("/", src.join("root")).unwrap();
        let raw = serde_json::json!({"services": {"api": {"image": "api", "volumes": [
            {"type": "bind", "source": src.join("root").to_string_lossy(), "target": "/h"}]}}});
        let model: Model = serde_json::from_value(raw.clone()).unwrap();
        let e = render(raw, &model, &src, &[], &d.path().join("files")).unwrap_err();
        assert!(e.contains("outside the lane"), "{e}");
    }

    #[test]
    fn a_group_names_files_inside_the_lane_only() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("compose.yml"), "services: {}\n").unwrap();
        let g = |files: Vec<std::path::PathBuf>, env: Vec<std::path::PathBuf>| Group {
            name: "app".into(),
            project: "p".into(),
            files,
            env_files: env,
            networks: vec![],
            services: vec![],
            dir: src.clone(),
            env: Default::default(),
        };
        assert!(
            check_group(
                &g(vec![src.join("compose.yml")], vec![src.join(".env")]),
                &src
            )
            .is_ok()
        );
        assert!(check_group(&g(vec![src.join("../x/compose.yml")], vec![]), &src).is_err());
        assert!(
            check_group(
                &g(vec![src.join("compose.yml")], vec![d.path().join(".env")]),
                &src
            )
            .is_err()
        );
    }
}
