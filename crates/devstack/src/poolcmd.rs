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
fn member_dir() -> PathBuf {
    devstack_node::member::default_dir(&data_dir())
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
        "{:<18}{:<10}{:<8}{:<24}{:<18}ASKED",
        "ID", "STATE", "ROLE", "NAME", "FROM"
    );
    for m in &ms {
        println!(
            "{:<18}{:<10}{:<8}{:<24}{:<18}{}",
            m.id,
            m.state.as_str(),
            if m.state == State::Approved {
                m.role.as_str()
            } else {
                "-"
            },
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

pub fn join(coordinator: &str, fingerprint: &str, name: Option<&str>, wait: bool) -> Result<()> {
    let local = Local::new(&member_dir());
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
            println!("`devstack join` again (or `devstack pool`) once approved");
            return Ok(());
        }
        println!("waiting for the approval (Ctrl-C stops waiting; the request stays)...");
        while m.state == State::Pending {
            std::thread::sleep(Duration::from_secs(3));
            m = local.refresh()?;
        }
    }
    match m.state {
        State::Approved => println!("joined the pool at {} as {} ({name})", m.coordinator, m.id),
        State::Removed => bail!(
            "the coordinator removed this machine ({}) from the pool",
            m.id
        ),
        State::Pending => unreachable!("waited above"),
    }
    Ok(())
}

pub fn pool() -> Result<()> {
    let local = Local::new(&member_dir());
    let m = match local.membership()? {
        Some(m) if m.state != State::Approved => local.refresh()?,
        Some(m) => m,
        None => {
            bail!("this machine is in no pool: `devstack join <coordinator> --fingerprint <fp>`")
        }
    };
    let nodes = local.nodes()?;
    println!(
        "pool at {} (this machine: {} {})",
        m.coordinator, m.id, m.name
    );
    println!(
        "{:<18}{:<24}{:<9}{:<10}{:<12}SEEN",
        "ID", "NAME", "STATE", "CPU", "MEMORY"
    );
    for n in &nodes {
        let (cpu, mem) = match n.report.as_ref().filter(|r| r.sharing) {
            Some(r) => (
                format!("{:.1}", r.cpus),
                format!("{:.1} GiB", r.memory as f64 / (1u64 << 30) as f64),
            ),
            None => ("-".into(), "-".into()),
        };
        println!(
            "{:<18}{:<24}{:<9}{:<10}{:<12}{}",
            n.id,
            n.name,
            if n.online { "online" } else { "offline" },
            cpu,
            mem,
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
    listen: &str,
    api: Option<String>,
    max_cpus: Option<f64>,
    max_memory_gib: Option<f64>,
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
    let dir = member_dir();
    let m = Local::new(&dir)
        .membership()?
        .filter(|m| m.state == State::Approved)
        .ok_or_else(|| {
            anyhow!("this machine is no approved member of a pool: `devstack join` first")
        })?;
    println!(
        "node agent of {} ({}) on {addr}; requesters reach it at {api}",
        m.name, m.id
    );
    devstack_node::node::serve_blocking(devstack_node::node::NodeConfig { dir, api, share }, addr)?;
    Ok(())
}
