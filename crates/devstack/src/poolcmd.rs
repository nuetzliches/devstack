//! `devstack node`, `devstack join`, `devstack pool`: the pool of machines
//! that run lanes (ADR-0025, ADR-0031). The coordinator is `devstack node
//! serve` on an always-on host; every other machine joins it once.

use anyhow::{Result, anyhow, bail};
use devstack_node::coordinator;
use devstack_node::member::Local;
use devstack_node::store::{Role, SHELL, State, Store};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn data_dir() -> PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

/// The coordinator's directory: `--dir`, else `DEVSTACK_COORDINATOR_DIR`,
/// else `coordinator/` in devstack's per-user state.
pub fn coordinator_dir(dir: Option<PathBuf>) -> PathBuf {
    dir.or_else(|| std::env::var_os("DEVSTACK_COORDINATOR_DIR").map(PathBuf::from))
        .unwrap_or_else(|| data_dir().join("coordinator"))
}

/// This machine's membership: `pool/` in devstack's per-user state.
/// The membership of `pool` (ADR-0034).
fn member_dir(pool: &str) -> Result<PathBuf> {
    if !devstack_node::member::valid_pool_name(pool) {
        bail!("--pool {pool}: a-z, 0-9, `-` and `_`, at most 32");
    }
    Ok(devstack_node::member::pool_dir(&data_dir(), pool))
}

/// ` --pool <name>` for messages, empty for the default pool.
fn pool_arg(pool: &str) -> String {
    if pool == devstack_node::member::DEFAULT_POOL {
        String::new()
    } else {
        format!(" --pool {pool}")
    }
}

fn need_coordinator(dir: &Path) -> Result<()> {
    if !dir.join("ca.key").exists() {
        bail!(
            "no coordinator in {}: `devstack node init --address <address>` first",
            dir.display()
        );
    }
    Ok(())
}

fn fingerprint(dir: &Path) -> Result<String> {
    Ok(devstack_node::pki::fingerprint(
        &coordinator::ca(dir)?.cert,
    )?)
}

fn join_hint(dir: &Path, port: Option<u16>) -> Result<String> {
    let host = coordinator::names(dir)?
        .into_iter()
        .next()
        .unwrap_or_else(|| "<address>".into());
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let port = port.map_or("<port>".into(), |p| p.to_string());
    Ok(format!(
        "devstack join https://{host}:{port} --fingerprint {}",
        fingerprint(dir)?
    ))
}

pub fn init(dir: Option<PathBuf>, addresses: &[String]) -> Result<()> {
    let dir = coordinator_dir(dir);
    let fp = coordinator::init(&dir, addresses)?;
    println!("coordinator created in {}", dir.display());
    println!("CA fingerprint: {fp}");
    println!(
        "start it with `devstack node serve --listen <address>:<port>`; members then join with:"
    );
    println!("  {}", join_hint(&dir, None)?);
    Ok(())
}

pub fn serve(dir: Option<PathBuf>, listen: &str) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let addr: std::net::SocketAddr = listen.parse().map_err(|e| {
        anyhow!("--listen {listen}: {e} (an address and a port, e.g. 10.0.0.5:4260)")
    })?;
    println!("coordinator listening on {addr} ({})", dir.display());
    println!("members join with: {}", join_hint(&dir, Some(addr.port()))?);
    coordinator::serve_blocking(&dir, addr)?;
    Ok(())
}

pub fn print_fingerprint(dir: Option<PathBuf>) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    println!("{}", fingerprint(&dir)?);
    Ok(())
}

fn ago(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s} s ago"),
        s if s < 7200 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

pub fn members_list(dir: Option<PathBuf>) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let ms = Store::new(&dir).list()?;
    if ms.is_empty() {
        println!(
            "no members yet; a machine asks with `{}`",
            join_hint(&dir, None)?
        );
        return Ok(());
    }
    let now = devstack_node::store::now();
    println!(
        "{:<18}{:<10}{:<8}{:<6}{:<24}{:<18}ASKED",
        "ID", "STATE", "ROLE", "NODE", "NAME", "FROM"
    );
    for m in &ms {
        println!(
            "{:<18}{:<10}{:<8}{:<6}{:<24}{:<18}{}",
            m.id,
            m.state.as_str(),
            if m.state == State::Approved {
                m.role.as_str()
            } else {
                "-"
            },
            if m.node { "yes" } else { "-" },
            m.name,
            m.address.as_deref().unwrap_or("-"),
            ago(now.saturating_sub(m.requested_at))
        );
    }
    let pending = ms.iter().filter(|m| m.state == State::Pending).count();
    if pending > 0 {
        println!("\n{pending} pending: `devstack node members approve <id>` admits one");
    }
    let admins = ms
        .iter()
        .filter(|m| m.state == State::Approved && m.role == Role::Admin)
        .count();
    if admins == 0 && ms.iter().any(|m| m.state == State::Approved) {
        println!(
            "no admin yet: `devstack node members role <id> admin` lets one manage the pool in the dashboard"
        );
    }
    Ok(())
}

pub fn members_approve(dir: Option<PathBuf>, member: &str, admin: bool) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let role = admin.then_some(Role::Admin);
    let m = Store::new(&dir).approve(member, &coordinator::ca(&dir)?, role, SHELL)?;
    println!(
        "approved {} ({}) as {}; it picks up its certificate by itself",
        m.id,
        m.name,
        m.role.as_str()
    );
    Ok(())
}

pub fn members_role(dir: Option<PathBuf>, member: &str, role: &str) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let role: Role = role.parse().map_err(|e: String| anyhow!(e))?;
    let m = Store::new(&dir).set_role(member, role, SHELL)?;
    println!("{} ({}) is {} now", m.id, m.name, m.role.as_str());
    Ok(())
}

/// `devstack node members node <id> on|off`: whether the pool runs lanes
/// there (ADR-0032, addendum 2026-10-17).
pub fn members_node(dir: Option<PathBuf>, member: &str, state: &str) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let on = match state {
        "on" => true,
        "off" => false,
        o => bail!("`{o}`: on or off"),
    };
    let m = Store::new(&dir).set_node(member, on, SHELL)?;
    if m.node {
        println!(
            "{} ({}) runs the pool's lanes once its node agent reports",
            m.id, m.name
        );
    } else {
        println!("{} ({}) runs no lanes for the pool", m.id, m.name);
    }
    Ok(())
}

pub fn members_tags(dir: Option<PathBuf>, member: &str, tags: &[String]) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let m = Store::new(&dir).set_tags(member, tags, SHELL)?;
    if m.tags.is_empty() {
        println!("{} ({}) has no tags", m.id, m.name);
    } else {
        println!("{} ({}): {}", m.id, m.name, m.tags.join(", "));
    }
    Ok(())
}

pub fn members_policy(
    dir: Option<PathBuf>,
    node: &str,
    allow: Vec<String>,
    repos: Vec<String>,
    (memory_headroom, max_cpus): (Option<f64>, Option<f64>),
    network: Option<String>,
    clear: bool,
) -> Result<()> {
    use devstack_node::policy::{Network, NodePolicy};
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let store = Store::new(&dir);
    let changes = clear
        || !allow.is_empty()
        || !repos.is_empty()
        || memory_headroom.is_some()
        || max_cpus.is_some()
        || network.is_some();
    let m = if !changes {
        let ms = store.list()?;
        ms.into_iter()
            .find(|m| m.id.starts_with(node) || m.name == node)
            .ok_or_else(|| anyhow!("no member `{node}`"))?
    } else if clear {
        store.set_policy(node, None, SHELL)?
    } else {
        let network = match network.as_deref() {
            None | Some("lan") => Network::Lan,
            Some("internet") => Network::Internet,
            Some("none") => Network::None,
            Some(o) => bail!("--network {o}: `lan`, `internet` or `none`"),
        };
        store.set_policy(
            node,
            Some(NodePolicy {
                allow,
                repos,
                memory_headroom,
                max_cpus,
                network,
                ..NodePolicy::default()
            }),
            SHELL,
        )?
    };
    match &m.policy {
        None => println!(
            "{} ({}): no policy, every member may run any lane there",
            m.id, m.name
        ),
        Some(p) => println!(
            "{} ({}): allow {}; repositories {}; memory {}; CPU {}; network {}",
            m.id,
            m.name,
            if p.allow.is_empty() {
                "everyone".into()
            } else {
                p.allow.join(", ")
            },
            if p.repos.is_empty() {
                "any".into()
            } else {
                p.repos.join(", ")
            },
            p.memory_headroom
                .map_or("unlimited".into(), |h| format!("need x {h}")),
            p.max_cpus
                .map_or("unlimited".into(), |c| format!("{c} cores per lane")),
            match p.network {
                Network::Lan => "the internet and the local network",
                Network::Internet => "the internet, not the local network",
                Network::None => "only the lane's own containers",
            }
        ),
    }
    Ok(())
}

pub fn members_remove(dir: Option<PathBuf>, member: &str) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let m = Store::new(&dir).remove(member, SHELL)?;
    println!(
        "removed {} ({}); its certificate is refused from now on",
        m.id, m.name
    );
    Ok(())
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "member".into())
}

pub fn join(
    pool: &str,
    coordinator: &str,
    fingerprint: &str,
    name: Option<&str>,
    wait: bool,
) -> Result<()> {
    let local = Local::new(&member_dir(pool)?);
    // Asking again keeps the name it asked with.
    let name = match name {
        Some(n) => n.to_string(),
        None => local.membership()?.map_or_else(hostname, |m| m.name),
    };
    let mut m = local.join(coordinator, fingerprint, &name)?;
    if m.state == State::Pending {
        println!(
            "asked to join as {} ({name}); on the coordinator, an administrator runs:\n  devstack node members approve {}",
            m.id, m.id
        );
        if !wait {
            println!(
                "`devstack join{}` again (or `devstack pool`) once approved",
                pool_arg(pool)
            );
            return Ok(());
        }
        println!("waiting for the approval (Ctrl-C stops waiting; the request stays)...");
        while m.state == State::Pending {
            std::thread::sleep(Duration::from_secs(3));
            m = local.refresh()?;
        }
    }
    match m.state {
        State::Approved => println!(
            "joined the pool `{pool}` at {} as {} ({name})",
            m.coordinator, m.id
        ),
        State::Removed => bail!(
            "the coordinator removed this machine ({}) from the pool",
            m.id
        ),
        State::Pending => unreachable!("waited above"),
    }
    Ok(())
}

/// `devstack pool telemetry [on|off]` (ADR-0037).
pub fn telemetry(pool: &str, state: Option<&str>) -> Result<()> {
    use devstack_node::telemetry::Consent;
    let dir = member_dir(pool)?;
    match state {
        Some("on") => Consent { enabled: true }.save(&dir)?,
        Some("off") => Consent { enabled: false }.save(&dir)?,
        Some(o) => bail!("`{o}`: `on` or `off`"),
        None => {}
    }
    let consent = Consent::load(&dir).enabled;
    let local = Local::new(&dir);
    let info = if local.membership()?.is_some() {
        local.telemetry_info().ok()
    } else {
        None
    };
    println!(
        "pool `{pool}`: this machine {} telemetry; the pool {}",
        if consent { "sends" } else { "sends no" },
        match &info {
            Some(i) if i.enabled => "collects it",
            Some(_) => "collects none (its coordinator has no collector)",
            None => "did not answer",
        }
    );
    if let Some(e) = info.as_ref().and_then(|i| i.last_error.as_ref()) {
        println!("the coordinator could not pass the last of it on: {e}");
    }
    if consent || state.is_none() {
        println!("what is sent:");
        for w in devstack_node::telemetry::collects() {
            println!("  - {w}");
        }
    }
    Ok(())
}

/// `devstack pool`: every pool this machine belongs to, or the one named.
pub fn pool(only: Option<&str>) -> Result<()> {
    let all = devstack_node::member::pools(&data_dir());
    let mut shown = 0;
    for (name, dir) in &all {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        let local = Local::new(dir);
        if local.membership()?.is_none() {
            continue;
        }
        if shown > 0 {
            println!();
        }
        shown += 1;
        if let Err(e) = pool_one(name, &local) {
            println!("pool `{name}`: {e}");
        }
    }
    if shown == 0 {
        match only {
            Some(p) => bail!(
                "this machine is in no pool `{p}`: `devstack join <coordinator> --fingerprint <fp>{}`",
                pool_arg(p)
            ),
            None => bail!(
                "this machine is in no pool: `devstack join <coordinator> --fingerprint <fp>`"
            ),
        }
    }
    Ok(())
}

fn pool_one(pool: &str, local: &Local) -> Result<()> {
    let m = match local.membership()? {
        Some(m) if m.state != State::Approved => local.refresh()?,
        Some(m) => m,
        None => unreachable!("checked by the caller"),
    };
    let nodes = local.nodes()?;
    println!(
        "pool `{pool}` at {} (this machine: {} {})",
        m.coordinator, m.id, m.name
    );
    println!(
        "{:<18}{:<24}{:<9}{:<10}{:<12}{:<10}SEEN",
        "ID", "NAME", "STATE", "CPU", "MEMORY", "DEVSTACK"
    );
    for n in &nodes {
        let (cpu, mem) = match n.report.as_ref().filter(|r| r.sharing) {
            Some(r) => (
                format!("{:.1}", r.cpus),
                format!("{:.1} GiB", r.memory as f64 / (1u64 << 30) as f64),
            ),
            None => ("-".into(), "-".into()),
        };
        // Its release; `older`: one that does not say (ADR-0031 point 8).
        let release = match n.report.as_ref() {
            Some(r) => r
                .devstack
                .as_ref()
                .map_or("older".into(), |d| d.version.clone()),
            None => "-".into(),
        };
        println!(
            "{:<18}{:<24}{:<9}{:<10}{:<12}{:<10}{}",
            n.id,
            n.name,
            if n.online { "online" } else { "offline" },
            cpu,
            mem,
            release,
            n.seen.map_or("never".into(), ago)
        );
    }
    let leases: Vec<devstack_node::coordinator::Lease> = local.get("/v1/leases")?;
    if !leases.is_empty() {
        let name = |id: &str| {
            nodes
                .iter()
                .find(|n| n.id == id)
                .map_or(id.to_string(), |n| n.name.clone())
        };
        println!(
            "\n{:<18}{:<24}{:<24}{:<10}LANE",
            "LEASE", "NODE", "FOR", "STATE"
        );
        for l in &leases {
            println!(
                "{:<18}{:<24}{:<24}{:<10}{}/{}",
                l.id,
                name(&l.node),
                name(&l.member),
                l.state,
                l.repo,
                l.lane
            );
        }
    }
    Ok(())
}

pub fn agent(
    pool: &str,
    listen: &str,
    api: Option<String>,
    max_cpus: Option<f64>,
    max_memory_gib: Option<f64>,
    dedicated: bool,
) -> Result<()> {
    let addr: std::net::SocketAddr = listen.parse().map_err(|e| {
        anyhow!("--listen {listen}: {e} (an address and a port, e.g. 10.0.0.5:4261)")
    })?;
    if addr.ip().is_unspecified() && api.is_none() {
        bail!(
            "--listen {addr} is every address: name the one requesters reach with --api https://<address>:{}",
            addr.port()
        );
    }
    let api = api.unwrap_or_else(|| format!("https://{addr}"));
    let share = devstack_node::share::Settings {
        enabled: true,
        max_cpus,
        max_memory: max_memory_gib.map(|g| (g * f64::from(1u32 << 30)) as u64),
    };
    let dir = member_dir(pool)?;
    let m = Local::new(&dir)
        .membership()?
        .filter(|m| m.state == State::Approved)
        .ok_or_else(|| {
            anyhow!(
                "this machine is no approved member of the pool `{pool}`: `devstack join{}` first",
                pool_arg(pool)
            )
        })?;
    println!(
        "node agent of {} ({}) in the pool `{pool}` on {addr}; requesters reach it at {api}",
        m.name, m.id
    );
    devstack_node::node::serve_blocking(
        devstack_node::node::NodeConfig {
            dir,
            api,
            share,
            dedicated,
        },
        addr,
    )?;
    Ok(())
}

/// `devstack node run-lane`: a lane on a node, driven from here without the
/// daemon (a smoke test for M2b).
pub fn run_lane(file: &Path, on: &str, lane: &str, keep: bool) -> Result<()> {
    use devstack_node::coordinator::LeaseRequest;
    use devstack_node::nodeclient::NodeClient;
    let file = std::path::absolute(file)?;
    let repo = file
        .parent()
        .ok_or_else(|| anyhow!("{} has no directory", file.display()))?
        .to_path_buf();
    let stack = devstack_config::load(&file).map_err(|e| anyhow!("{e}"))?;
    let (pool, node) =
        devstack_node::member::resolve_on(&data_dir(), on).map_err(|e| anyhow!("{e}"))?;
    let local = Local::new(&member_dir(&pool)?);
    let offer = local.lease(&LeaseRequest {
        node,
        repo: stack.stack.name.clone(),
        lane: lane.into(),
        need: devstack_node::share::Amount {
            cpus: 0.5,
            memory: 1 << 30,
        },
        devstack: Some(devstack_node::version::Release::this()),
    })?;
    println!(
        "lease {} on {} ({})",
        offer.id, offer.node.name, offer.node.api
    );
    let client = NodeClient::new(&local, offer.node.clone())?;
    let rt = tokio::runtime::Runtime::new()?;
    let id = offer.id.clone();
    let result = rt.block_on(async {
        client.admit(&offer).await?;
        let tmp = std::env::temp_dir().join(format!("devstack-run-lane-{id}"));
        let files = devstack_build::snapshot::Snapshot::open(&repo, &tmp)?.list()?;
        let t0 = std::time::Instant::now();
        let s = client.sync(&id, &repo, &files).await?;
        println!(
            "synced {} files ({} uploaded, {} KiB) in {:.1} s",
            s.files,
            s.uploaded,
            s.bytes / 1024,
            t0.elapsed().as_secs_f64()
        );
        let rel = file
            .strip_prefix(&repo)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| "devstack.toml".into());
        let t0 = std::time::Instant::now();
        client
            .start(
                &id,
                &rel,
                None,
                0,
                Duration::from_secs(1800),
                &|p| println!("  {p}"),
                &|p, ms| println!("    ({p}: {:.1} s)", ms as f64 / 1000.0),
            )
            .await?;
        println!(
            "lane started in {:.1} s; starting its services",
            t0.elapsed().as_secs_f64()
        );
        let (st, body) = client.api(&id, "POST", "/v1/up", None).await?;
        if st >= 300 {
            bail!("POST /v1/up: {st} {body}");
        }
        let until = std::time::Instant::now() + Duration::from_secs(600);
        let mut last = String::new();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            client.renew(&id).await?;
            let (st, body) = client.api(&id, "GET", "/v1/stack", None).await?;
            if st != 200 {
                println!("GET /v1/stack: {st}");
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&body)?;
            let states: Vec<String> = v["services"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    format!(
                        "{}={}",
                        s["name"].as_str().unwrap_or("?"),
                        s["status"]["state"].as_str().unwrap_or("?")
                    )
                })
                .collect();
            let line = states.join(" ");
            if line != last {
                println!("[{:>4} s] {line}", (t0.elapsed().as_secs()));
                last = line;
            }
            let settled = states.iter().all(|s| {
                s.ends_with("=healthy") || s.ends_with("=failed") || s.ends_with("=stopped")
            });
            if settled || std::time::Instant::now() > until {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    });
    if keep {
        println!("lease {id} stays; it expires within two minutes unless renewed");
    } else {
        let _ = rt.block_on(client.release(&id));
        println!("released lease {id}");
    }
    result
}
