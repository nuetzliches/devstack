//! Containers of a stack whose daemon does not run (ADR-0096): Docker
//! starts them again by their restart policy when it starts, or a `down`
//! that kept them left them. They hold the stack's ports all the same, so
//! the port check before `up`, `/hub/ports` and `doctor` count them, and
//! `devstack down` removes them without a daemon.
//!
//! A container belongs to the stack whose Compose project it carries: a
//! group's `project` for the main tree, `<project>-<lane>` for a lane. Of
//! several stacks' projects the longest that fits wins (`shop-dev-search`
//! before `shop-dev`).

use crate::ports::{PUBLISHED, PortEntry, VIA_DOCKER};
use devstack_compose::{Docker, ProjectPort};
use std::collections::BTreeMap;

/// A Compose project of a registered stack: the stack's id, the group, the
/// project of its main tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub group: String,
    pub project: String,
}

/// The main tree's Compose projects of `stack` (registered as `id`).
pub fn projects(id: &str, stack: &devstack_config::Stack) -> Vec<Project> {
    stack
        .compose
        .iter()
        .map(|(g, cfg)| Project {
            id: id.to_string(),
            group: g.clone(),
            project: cfg.project.clone(),
        })
        .collect()
}

/// Whose `project` is: the project it is or the lane of, and the lane
/// (`main`, or what follows `<project>-`); the longest that fits.
pub fn owner<'a>(known: &'a [Project], project: &str) -> Option<(&'a Project, String)> {
    known
        .iter()
        .filter_map(|p| {
            if project == p.project {
                Some((p, "main".to_string()))
            } else {
                let lane = project.strip_prefix(&p.project)?.strip_prefix('-')?;
                (!lane.is_empty()).then(|| (p, lane.to_string()))
            }
        })
        .max_by_key(|(p, _)| p.project.len())
}

/// The ports `running` (every running Compose container's) publish, by the
/// stack id of `known` they belong to, as `/hub/ports` lists them: kind
/// `published`, held by `<group>.<service>`, listening.
pub fn entries(known: &[Project], running: &[ProjectPort]) -> BTreeMap<String, Vec<PortEntry>> {
    let mut out: BTreeMap<String, Vec<PortEntry>> = BTreeMap::new();
    for r in running {
        let Some((p, lane)) = owner(known, &r.project) else {
            continue;
        };
        out.entry(p.id.clone()).or_default().push(PortEntry {
            port: r.port,
            proto: r.proto.clone(),
            lane,
            kind: PUBLISHED.into(),
            holder: Some(format!("{}.{}", p.group, r.service)),
            via: VIA_DOCKER.into(),
            target: Some(r.target),
            target_lane: None,
            node: None,
            listening: true,
        });
    }
    out
}

/// What every running Compose container on this machine publishes; empty
/// when Docker does not answer (then no container holds a port either).
pub fn running() -> Vec<ProjectPort> {
    Docker::clean().published_by_projects().unwrap_or_default()
}

/// Remove the containers of `stack`'s main tree (and with `lanes` its
/// lanes' too) that Docker has, volumes kept: `devstack down` while no
/// daemon of it runs. The projects it removed.
pub fn remove(stack: &devstack_config::Stack, lanes: bool) -> Result<Vec<String>, String> {
    let known = projects("", stack);
    let docker = Docker::clean();
    let mut done = Vec::new();
    for name in crate::lanes::compose_project_names() {
        let Some((_, lane)) = owner(&known, &name) else {
            continue;
        };
        if lane != "main" && !lanes {
            continue;
        }
        docker.down_project(&name).map_err(|e| e.to_string())?;
        done.push(name);
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, group: &str, project: &str) -> Project {
        Project {
            id: id.into(),
            group: group.into(),
            project: project.into(),
        }
    }

    fn port(project: &str, service: &str, port: u16) -> ProjectPort {
        ProjectPort {
            project: project.into(),
            service: service.into(),
            port,
            target: port,
            proto: "tcp".into(),
        }
    }

    #[test]
    fn a_container_belongs_to_the_longest_project_that_fits() {
        let known = vec![
            p("shop", "app", "shop-dev"),
            p("shop", "search", "shop-dev-search"),
            p("docs", "app", "docs"),
        ];
        let (o, lane) = owner(&known, "shop-dev").unwrap();
        assert_eq!((o.group.as_str(), lane.as_str()), ("app", "main"));
        let (o, lane) = owner(&known, "shop-dev-search-fix-1").unwrap();
        assert_eq!((o.group.as_str(), lane.as_str()), ("search", "fix-1"));
        let (o, lane) = owner(&known, "shop-dev-fix-1").unwrap();
        assert_eq!((o.group.as_str(), lane.as_str()), ("app", "fix-1"));
        assert!(owner(&known, "shopping").is_none());
        assert!(owner(&known, "docs-").is_none());
    }

    #[test]
    fn the_ports_of_running_containers_go_to_their_stack() {
        let known = vec![p("shop", "app", "shop-dev"), p("docs", "app", "docs")];
        let got = entries(
            &known,
            &[
                port("shop-dev", "otel", 4317),
                port("shop-dev-fix", "db", 5532),
                port("elsewhere", "x", 9999),
            ],
        );
        assert_eq!(got.len(), 1, "{got:?}");
        let shop = &got["shop"];
        assert_eq!(shop.len(), 2);
        assert_eq!(shop[0].port, 4317);
        assert_eq!(shop[0].lane, "main");
        assert_eq!(shop[0].kind, PUBLISHED);
        assert_eq!(shop[0].holder.as_deref(), Some("app.otel"));
        assert!(shop[0].listening);
        assert_eq!(shop[1].lane, "fix");
    }
}
