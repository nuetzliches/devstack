//! `devstack env <service>`: the environment a native service would get, with
//! the origin of every variable, or exported as a dotenv file (ADR-0014,
//! ADR-0015). With a daemon running it asks the daemon, which knows what it
//! starts the service with (in a worktree: the lane's ports, containers and
//! names); without one it reads Compose itself (needs Docker).

use anyhow::{Context as _, Result, bail};
use devstack_compose::{Docker, Group};
use devstack_config::Stack;
use devstack_env::{ComposeEnvs, Context};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Compose data needed by one service: its own group's environment and the
/// published addresses of every group it refers to.
pub fn context_for(
    stack: &Stack,
    repo: &Path,
    name: &str,
    docker: &Docker,
) -> Result<(Context, ComposeEnvs)> {
    let svc = stack
        .services
        .get(name)
        .with_context(|| format!("no service `{name}` in the stack file"))?;
    let mut ctx = Context {
        repo: repo.to_path_buf(),
        port: svc.port,
        is_main: true,
        ..Default::default()
    };
    // Without a daemon this is the main tree: each group's own project.
    for (g, cfg) in &stack.compose {
        ctx.projects.insert(g.clone(), cfg.project.clone());
    }
    for (n, s) in &stack.services {
        if let Some(p) = s.port {
            let tls = s.health.as_ref().is_some_and(|h| h.insecure_tls);
            ctx.service_urls.insert(
                n.clone(),
                format!("{}://127.0.0.1:{p}", if tls { "https" } else { "http" }),
            );
        }
    }

    // Groups whose data is needed: the service's own and every rewritten host's.
    let mut groups: BTreeSet<String> = BTreeSet::new();
    if let Some(r) = &svc.compose {
        groups.insert(r.group.clone());
        for h in &svc.rewrite_hosts {
            if let Some((g, _)) = h.split_once('.') {
                groups.insert(g.to_string());
            }
        }
    }
    for v in svc.env.values() {
        if let Ok(segs) = devstack_config::interpolate::parse(v) {
            for p in devstack_config::interpolate::placeholders(&segs) {
                if let devstack_config::interpolate::Placeholder::ComposePort { group, .. } = p {
                    groups.insert(group.clone());
                }
            }
        }
    }

    // `${VAR}`: the stack's env files, the group's, then the process environment.
    ctx.vars
        .extend(devstack_env::variables(stack, Some(svc), repo));

    let mut envs = ComposeEnvs::new();
    for g in &groups {
        let group = Group::from_config(g, &stack.compose[g], repo);
        let model = docker
            .config(&group)
            .with_context(|| format!("compose group `{g}`"))?;
        // Statically published ports from the files, then what is actually running.
        for (svc_name, m) in &model.services {
            for p in &m.ports {
                let key = (g.clone(), svc_name.clone(), p.target);
                match &p.published {
                    Some(published) => {
                        ctx.published.insert(key, format!("127.0.0.1:{published}"));
                    }
                    // Docker assigns the port at start; `ps` below fills it in if running.
                    None => {
                        ctx.pending.insert(key);
                    }
                }
            }
            envs.insert((g.clone(), svc_name.clone()), m.environment.clone());
        }
        if let Ok(running) = docker.ps(&group) {
            for c in running {
                for p in &c.publishers {
                    if let Some(addr) = c.published(p.target_port) {
                        ctx.published
                            .insert((g.clone(), c.service.clone(), p.target_port), addr);
                    }
                }
            }
        }
    }
    Ok((ctx, envs))
}

pub fn run(
    stack: &Stack,
    repo: &Path,
    stack_file: &Path,
    name: &str,
    export: bool,
    out: Option<&Path>,
) -> Result<()> {
    // The export is steps 1-3 only (ADR-0015); the listing shows step 4 too.
    let env: Vec<(String, String, String)> = match from_daemon(stack_file, name, export)? {
        Some(env) => env,
        None => {
            let docker = Docker::default();
            let (ctx, envs) = context_for(stack, repo, name, &docker)?;
            let svc = &stack.services[name];
            devstack_env::compose(name, svc, &ctx, &envs, !export)?
                .into_iter()
                .map(|(k, v)| (k, v.value, v.origin.to_string()))
                .collect()
        }
    };
    if export {
        let text = devstack_env::to_dotenv(
            &env.iter()
                .map(|(k, v, _)| {
                    (
                        k.clone(),
                        devstack_env::Var {
                            value: v.clone(),
                            origin: devstack_env::Origin::StackFile,
                        },
                    )
                })
                .collect(),
        );
        match out {
            Some(p) => {
                std::fs::write(p, text).with_context(|| format!("write {}", p.display()))?;
                eprintln!(
                    "{}: {} variable(s); keep this file out of version control",
                    p.display(),
                    env.len()
                );
            }
            None => print!("{text}"),
        }
        return Ok(());
    }
    if env.is_empty() {
        bail!("`{name}` gets no variables from devstack (no `compose`, `env` or `workers`)");
    }
    let width = env.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);
    let mut by_origin: BTreeMap<String, usize> = BTreeMap::new();
    for (k, v, origin) in &env {
        println!("{k:width$} = {v}    # {origin}");
        *by_origin
            .entry(origin.split(',').next().unwrap_or("").to_string())
            .or_default() += 1;
    }
    eprintln!(
        "{} variable(s): {}",
        env.len(),
        by_origin
            .iter()
            .map(|(o, n)| format!("{n} from {o}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

/// The environment from the running daemon (in a worktree: its lane's);
/// `None` without a daemon. A worktree whose lane is not started has no
/// environment of its own yet: saying the main tree's would mislead.
fn from_daemon(
    stack_file: &Path,
    name: &str,
    export: bool,
) -> Result<Option<Vec<(String, String, String)>>> {
    let Some(c) = crate::client::Client::connect(&crate::client::state_dir(stack_file)?) else {
        if crate::client::worktree().is_some() {
            bail!(
                "this worktree's lane is not running: `devstack up` here first; only the lane knows its ports and databases"
            );
        }
        return Ok(None);
    };
    if c.lane() == Some("-") {
        bail!(
            "this worktree's lane is not started: `devstack up` here first; only the lane knows its ports and databases"
        );
    }
    let body = c.get(&format!("/v1/services/{name}/env?export={export}"))?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body)
        .map_err(|_| anyhow::anyhow!("{}", crate::client::error_text(&body)))?;
    Ok(Some(
        rows.iter()
            .map(|r| {
                let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
                (s("name"), s("value"), s("origin"))
            })
            .collect(),
    ))
}
