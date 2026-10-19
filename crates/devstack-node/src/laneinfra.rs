//! A remote lane's containers, seen from its node (#101). The lane's daemon
//! has no Docker (ADR-0025 point 2): the agent answers what needs it, the
//! containers' output and their start, stop and restart, for the lane's
//! owner only, and keeps the file that describes them to the daemon current,
//! with what each uses, so its dashboard shows them as a local lane's.
//!
//! Containers are found by the Compose labels of the projects the agent
//! itself started for the lease (`infra.json` beside the mirror, which the
//! lane cannot write), and acted on by name: no Compose file of the mirror is
//! read here (ADR-0033, [`crate::confine`]).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// What the agent keeps of a lane's infrastructure, beside its mirror.
pub const AGENT_FILE: &str = "infra.json";
/// The file the lane's daemon reads (`DEVSTACK_EXTERNAL_INFRA`), in the mirror.
pub const LANE_FILE: &str = ".devstack/external-infra.json";
/// How often the agent writes states and load into it.
pub const EVERY: Duration = Duration::from_secs(5);

/// One container of a lane's Compose project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub service: String,
    pub name: String,
    pub state: String,
    pub health: String,
}

/// The Compose project of each group the agent started for a lease, from
/// its own file; `None` for a lease started by an older agent.
pub fn projects(dir: &Path) -> Option<BTreeMap<String, String>> {
    #[derive(Deserialize)]
    struct Infra {
        #[serde(default)]
        groups: BTreeMap<String, Group>,
    }
    #[derive(Deserialize)]
    struct Group {
        #[serde(default)]
        project: String,
    }
    let text = std::fs::read(dir.join(AGENT_FILE)).ok()?;
    let infra: Infra = serde_json::from_slice(&text).ok()?;
    Some(
        infra
            .groups
            .into_iter()
            .filter(|(_, g)| !g.project.is_empty())
            .map(|(name, g)| (name, g.project))
            .collect(),
    )
}

fn docker(args: &[&str], within: Duration) -> Result<std::process::Output, String> {
    let mut cmd = Command::new("docker");
    cmd.args(args);
    devstack_compose::output_within(cmd, within)
        .map_err(|e| format!("run docker: {e}"))?
        .ok_or_else(|| {
            format!(
                "docker {} did not finish within {} s",
                args.first().copied().unwrap_or(""),
                within.as_secs()
            )
        })
}

/// The health Docker puts into a container's status: `Up 3 minutes
/// (healthy)`, `(health: starting)`, `(unhealthy)`.
fn health_of(status: &str) -> String {
    if status.contains("(healthy)") {
        "healthy"
    } else if status.contains("(unhealthy)") {
        "unhealthy"
    } else if status.contains("health: starting") {
        "starting"
    } else {
        ""
    }
    .to_string()
}

/// Lines of `docker ps --format '{{.Label "…service"}}\t{{.Names}}\t{{.State}}\t{{.Status}}'`.
fn parse_ps(text: &str) -> Vec<Container> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (service, name, state) = (f.next()?, f.next()?, f.next()?);
            let status = f.next().unwrap_or("");
            (!service.is_empty() && !name.is_empty()).then(|| Container {
                service: service.to_string(),
                name: name.to_string(),
                state: state.to_string(),
                health: health_of(status),
            })
        })
        .collect()
}

/// Every container of Compose project `project`, running or not, by its labels.
pub fn containers(project: &str) -> Result<Vec<Container>, String> {
    let filter = format!("label=com.docker.compose.project={project}");
    let out = docker(
        &[
            "ps",
            "-a",
            "--filter",
            &filter,
            "--format",
            "{{.Label \"com.docker.compose.service\"}}\t{{.Names}}\t{{.State}}\t{{.Status}}",
        ],
        devstack_compose::budget::QUICK,
    )?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // A label filter is exact; the project label is checked again all the
    // same, it is what keeps one lease from another's containers.
    Ok(parse_ps(&String::from_utf8_lossy(&out.stdout)))
}

/// The container of `service` in group `group` of the lease whose mirror is
/// under `dir`: `Err` with a message for the lane's owner otherwise.
pub fn find(dir: &Path, group: &str, service: &str) -> Result<Container, String> {
    let projects = projects(dir).ok_or(
        "this lane's containers are not known on its node: it has not started there, or an older devstack started it (stop and start the lane)",
    )?;
    let project = projects
        .get(group)
        .ok_or_else(|| format!("no compose group `{group}` in this lane"))?;
    containers(project)?
        .into_iter()
        .find(|c| c.service == service)
        .ok_or_else(|| format!("no container of `{group}.{service}` in this lane"))
}

/// The last `tail` lines of a container's output, timestamped, stdout and
/// stderr in one order (as the daemon's `docker compose logs`).
pub fn logs(name: &str, tail: usize, since: Option<&str>) -> Result<String, String> {
    let tail_s = tail.to_string();
    let mut args = vec!["logs", "--timestamps", "--tail", &tail_s];
    if let Some(s) = since {
        args.extend(["--since", s]);
    }
    args.extend(["--", name]);
    let out = docker(&args, devstack_compose::budget::QUICK)?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(devstack_compose::merge_by_time(
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
        tail,
    ))
}

/// Follow a container's output: the last `tail` lines, then every new one,
/// timestamped, on the child's stdout and stderr. The caller ends it.
pub fn follow(name: &str, tail: usize) -> Result<std::process::Child, String> {
    Command::new("docker")
        .args(["logs", "--follow", "--timestamps", "--tail"])
        .arg(tail.to_string())
        .args(["--", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("run docker: {e}"))
}

/// `start`, `stop` or `restart` of one container, by its name.
pub fn act(name: &str, action: &str) -> Result<(), String> {
    let args: Vec<&str> = match action {
        "start" => vec!["start", "--", name],
        "stop" => vec!["stop", "--timeout", "10", "--", name],
        "restart" => vec!["restart", "--timeout", "10", "--", name],
        _ => return Err(format!("unknown action `{action}`: start, stop or restart")),
    };
    let out = docker(&args, devstack_compose::budget::STOP)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// What one container uses now, by name.
#[derive(Debug, Clone, Copy, Serialize)]
struct Load {
    cpu: f64,
    memory: u64,
}

/// The daemon's file with the states, health and load of now written into
/// it: `base` is the agent's own copy (what it wrote at start), `found` what
/// `docker ps` sees per group, `load` what `docker stats` measured.
fn current(
    base: &serde_json::Value,
    found: &BTreeMap<String, Vec<Container>>,
    load: &BTreeMap<String, Load>,
    at: u128,
) -> serde_json::Value {
    let mut out = base.clone();
    if let Some(groups) = out.get_mut("groups").and_then(|g| g.as_object_mut()) {
        for (group, g) in groups.iter_mut() {
            let Some(services) = g.get_mut("services").and_then(|s| s.as_object_mut()) else {
                continue;
            };
            for (service, s) in services.iter_mut() {
                let c = found
                    .get(group)
                    .and_then(|cs| cs.iter().find(|c| &c.service == service));
                // A service the agent did not start (parked, an environment
                // source only) stays as it was: its state is empty.
                if s["state"].as_str().is_some_and(str::is_empty) && c.is_none() {
                    continue;
                }
                // Gone (removed by hand): not shown, as a local lane's.
                s["state"] = c.map(|c| c.state.clone()).unwrap_or_default().into();
                s["health"] = c.map(|c| c.health.clone()).unwrap_or_default().into();
                match c.and_then(|c| load.get(&c.name)) {
                    Some(l) => {
                        s["cpu"] = l.cpu.into();
                        s["memory"] = l.memory.into();
                    }
                    None => {
                        if let Some(o) = s.as_object_mut() {
                            o.remove("cpu");
                            o.remove("memory");
                        }
                    }
                }
            }
        }
    }
    out["at"] = serde_json::Value::from(at as u64);
    out
}

/// Write the states and load of the lease's containers into the daemon's
/// file (`src` the mirror, `dir` the lease's directory beside it).
pub fn refresh(dir: &Path, src: &Path) -> Result<(), String> {
    let Ok(text) = std::fs::read(dir.join(AGENT_FILE)) else {
        return Ok(()); // started by an older agent
    };
    let base: serde_json::Value = serde_json::from_slice(&text).map_err(|e| e.to_string())?;
    let projects = projects(dir).unwrap_or_default();
    let mut found = BTreeMap::new();
    for (group, project) in &projects {
        found.insert(group.clone(), containers(project)?);
    }
    let running: Vec<String> = found
        .values()
        .flatten()
        .filter(|c| c.state == "running")
        .map(|c| c.name.clone())
        .collect();
    let load: BTreeMap<String, Load> = devstack_compose::Docker::default()
        .stats(&running)
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            (
                s.name,
                Load {
                    cpu: s.cpu,
                    memory: s.memory,
                },
            )
        })
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let json =
        serde_json::to_vec_pretty(&current(&base, &found, &load, now)).expect("infra serialises");
    crate::safefs::write_beneath(src, Path::new(LANE_FILE), 0o644, |f| {
        std::io::Write::write_all(f, &json)
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_come_from_docker_ps_with_their_health() {
        let ps = "db\tshop-ab12-db-1\trunning\tUp 3 minutes (healthy)\n\
                  api\tshop-ab12-api-1\texited\tExited (1) 2 minutes ago\n\
                  \tstray\trunning\tUp\n\
                  cache\tshop-ab12-cache-1\trunning\tUp 4 seconds (health: starting)\n";
        let c = parse_ps(ps);
        assert_eq!(
            c.len(),
            3,
            "a container without a service label is none of the lane's"
        );
        assert_eq!(c[0].health, "healthy");
        assert_eq!((c[1].state.as_str(), c[1].health.as_str()), ("exited", ""));
        assert_eq!(c[2].health, "starting");
    }

    #[test]
    fn the_daemons_file_gets_states_and_load_of_now() {
        let base = serde_json::json!({ "groups": { "app": { "project": "shop-ab12", "services": {
            "db": { "environment": { "X": "1" }, "ports": { "5432": "db:5432" }, "state": "running", "health": "starting" },
            "api": { "state": "running", "health": "" },
            "seed": { "state": "", "health": "" },
        } } } });
        let found = BTreeMap::from([(
            "app".to_string(),
            vec![Container {
                service: "db".into(),
                name: "shop-ab12-db-1".into(),
                state: "running".into(),
                health: "healthy".into(),
            }],
        )]);
        let load = BTreeMap::from([(
            "shop-ab12-db-1".to_string(),
            Load {
                cpu: 12.5,
                memory: 1 << 20,
            },
        )]);
        let v = current(&base, &found, &load, 42);
        let s = &v["groups"]["app"]["services"];
        assert_eq!(s["db"]["health"], "healthy");
        assert_eq!(s["db"]["cpu"], 12.5);
        assert_eq!(s["db"]["memory"], 1 << 20);
        assert_eq!(
            s["db"]["environment"]["X"], "1",
            "what the agent wrote at start stays"
        );
        assert_eq!(s["api"]["state"], "", "its container is gone");
        assert!(s["api"].get("cpu").is_none());
        assert_eq!(s["seed"]["state"], "", "a parked service stays parked");
        assert_eq!(v["at"], 42);
    }
}
