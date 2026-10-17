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
//! (bind mounts, build contexts, env files, secrets) must resolve beneath
//! the mirror on the node too (a symlink the lane put there does not).

use crate::policy::Network;
use crate::safefs::resolve_beneath;
use devstack_compose::{Group, Model};
use std::ffi::OsString;
use std::path::Path;

/// The image `compose config` runs in; `DEVSTACK_NODE_COMPOSE_IMAGE`
/// names another one (it needs `docker compose`).
pub fn config_image() -> String {
    std::env::var("DEVSTACK_NODE_COMPOSE_IMAGE")
        .ok()
        .filter(|i| !i.trim().is_empty())
        .unwrap_or_else(|| "docker:cli".into())
}

/// The arguments of `docker run` that read `g`'s model in a container that
/// sees `src` only. `flags` go to `config` (`--no-env-resolution`).
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
        "--entrypoint",
        "docker",
        image,
    ]
    .iter()
    .map(OsString::from)
    .collect();
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

/// Check a group's model (read with `--no-env-resolution`) for a lane on a
/// node under `network`: every problem, in words.
pub fn check_model(
    group: &str,
    model: &Model,
    src: &Path,
    network: Network,
    own_networks: &[String],
) -> Result<(), String> {
    let mut bad = Vec::new();
    let beneath = |what: &str, p: &str, bad: &mut Vec<String>| {
        if let Err(e) = resolve_beneath(src, Path::new(p)) {
            bad.push(format!("{what}: {e}"));
        }
    };
    for (name, s) in &model.services {
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
        }
        for (what, p) in paths {
            beneath(&format!("service `{name}`, {what}"), &p, &mut bad);
        }
    }
    for (key, v) in &model.volumes {
        let Some(v) = v else { continue };
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
        let Some(n) = n else { continue };
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
    for (kind, items) in [("secret", &model.secrets), ("config", &model.configs)] {
        for (key, f) in items {
            let Some(f) = f else { continue };
            if f.external {
                bad.push(format!("{kind} `{key}`: an external one"));
            }
            if let Some(p) = &f.file {
                beneath(&format!("{kind} `{key}`"), p, &mut bad);
            }
        }
    }
    finish(group, bad)
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
            check_model("app", &fine, &src, Network::Internet, &["edge".into()]).is_ok(),
            "{:?}",
            check_model("app", &fine, &src, Network::Internet, &["edge".into()])
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
        let e = check_model("app", &root, &src, Network::Internet, &[]).unwrap_err();
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
        let e = check_model("app", &root, &src, Network::Lan, &[]).unwrap_err();
        assert!(
            !e.contains("its own addresses") && !e.contains("IPv6"),
            "{e}"
        );
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
