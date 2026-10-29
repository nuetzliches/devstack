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

pub fn serve(dir: Option<PathBuf>, listen: &str, provisioner: Option<&str>) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let addr: std::net::SocketAddr = listen.parse().map_err(|e| {
        anyhow!("--listen {listen}: {e} (an address and a port, e.g. 10.0.0.5:4260)")
    })?;
    println!("coordinator listening on {addr} ({})", dir.display());
    println!("members join with: {}", join_hint(&dir, Some(addr.port()))?);
    let provisioner = provisioner.map(|url| devstack_node::provision::Provisioner::new(&dir, url));
    if let Some(p) = &provisioner {
        println!(
            "machines for lanes no node can take come from the provisioner at {}{} (ADR-0051); whoever runs it can read the lanes on them",
            p.url,
            if p.token.is_some() {
                ""
            } else {
                ", without a token (DEVSTACK_PROVISIONER_TOKEN or provisioner.token)"
            }
        );
    }
    coordinator::serve_blocking(
        &dir,
        addr,
        coordinator::Options {
            provisioner,
            ..coordinator::Options::default()
        },
    )?;
    Ok(())
}

fn on_off(state: &str) -> Result<bool> {
    match state {
        "on" => Ok(true),
        "off" => Ok(false),
        o => bail!("`{o}`: on or off"),
    }
}

/// `devstack node members provision <id> on|off`: whether a member's lane
/// that no node can take makes a machine (ADR-0051 point 3).
pub fn members_provision(dir: Option<PathBuf>, member: &str, state: &str) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let m = Store::new(&dir).set_provision(member, on_off(state)?, SHELL)?;
    if m.provision {
        println!(
            "{} ({}) may cause provisioning: a lane of it no node can take makes a machine, which costs money",
            m.id, m.name
        );
    } else {
        println!("{} ({}) causes no provisioning", m.id, m.name);
    }
    Ok(())
}

/// `devstack node confirm <node>`: an admin confirms a provisioned node
/// (ADR-0051 point 6).
pub fn confirm(dir: Option<PathBuf>, node: &str) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let m = Store::new(&dir).confirm(node, SHELL)?;
    let p = m.provisioned.as_ref().expect("a provisioned node");
    println!(
        "confirmed {} ({}): machine {} at {}, key {}; it takes lanes now",
        m.id,
        m.name,
        p.machine,
        p.address,
        m.cert_fp.as_deref().unwrap_or("-")
    );
    Ok(())
}

/// `devstack node give-back <node>`: its membership is revoked; the
/// coordinator has the provisioner delete its machine, its lanes with it.
pub fn give_back(dir: Option<PathBuf>, node: &str) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let store = Store::new(&dir);
    let ms = store.list()?;
    let Some(m) = ms
        .iter()
        .find(|m| (m.id.starts_with(node) || m.name == node) && m.provisioned.is_some())
    else {
        bail!("no provisioned node `{node}` (`devstack node provisioning` lists them)");
    };
    store.remove(&m.id, SHELL)?;
    println!(
        "removed {} ({}); the running coordinator has its machine deleted",
        m.id, m.name
    );
    Ok(())
}

/// `devstack node provisioning`: the pool's settings for provisioning
/// (ADR-0051), changed by the options given, and the machines.
pub fn provisioning(
    dir: Option<PathBuf>,
    per: Option<&str>,
    confirm: Option<&str>,
    idle_minutes: Option<u64>,
    max_nodes: Option<u32>,
    (max_cpus, max_memory_gib): (Option<f64>, Option<f64>),
) -> Result<()> {
    use devstack_node::provision::{self, Confirm, Per, Settings};
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    let mut s = Settings::load(&dir);
    let before = s.clone();
    if let Some(p) = per {
        s.per = match p {
            "pool" => Per::Pool,
            "repo" => Per::Repo,
            o => bail!("--per {o}: `pool` or `repo`"),
        };
    }
    if let Some(c) = confirm {
        s.confirm = match c {
            "none" => Confirm::None,
            "admin" => Confirm::Admin,
            o => bail!("--confirm {o}: `none` or `admin`"),
        };
    }
    if let Some(m) = idle_minutes {
        s.idle_minutes = m;
    }
    if let Some(n) = max_nodes {
        s.max_nodes = n;
    }
    if let Some(c) = max_cpus {
        s.max_cpus = (c > 0.0).then_some(c);
    }
    if let Some(m) = max_memory_gib {
        s.max_memory = (m > 0.0).then(|| (m * f64::from(1u32 << 30)) as u64);
    }
    if s != before {
        s.save(&dir)?;
        if let Some(admin) = Store::new(&dir)
            .list()?
            .into_iter()
            .find(|m| m.state == State::Approved && m.role == Role::Admin)
        {
            Store::new(&dir).record(
                SHELL,
                "provisioning",
                &admin.id,
                Some(serde_json::to_string(&s)?),
            )?;
        }
    }
    println!(
        "provisioned nodes run {}; {}; at most {} at once{}{}; idle ones are given back after {} min",
        match s.per {
            Per::Pool => "every member's lanes",
            Per::Repo => "the lanes of the repository they were made for",
        },
        match s.confirm {
            Confirm::None => "they take lanes once enrolled",
            Confirm::Admin => "an admin confirms each (`devstack node confirm <id>`)",
        },
        s.max_nodes,
        s.max_cpus
            .map_or(String::new(), |c| format!(", {c} CPU together")),
        s.max_memory.map_or(String::new(), |m| format!(
            ", {} together",
            coordinator::gib(m)
        )),
        s.idle_minutes
    );
    let machines = provision::load_machines(&dir);
    if machines.is_empty() {
        println!("no machines");
        return Ok(());
    }
    println!(
        "\n{:<10}{:<16}{:<16}{:<14}{:<18}NOTE",
        "REQUEST", "MACHINE", "ADDRESS", "STATE", "NODE"
    );
    for m in &machines {
        println!(
            "{:<10}{:<16}{:<16}{:<14}{:<18}{}",
            m.request,
            if m.machine.is_empty() {
                "-"
            } else {
                &m.machine
            },
            if m.address.is_empty() {
                "-"
            } else {
                &m.address
            },
            format!("{:?}", m.state).to_lowercase(),
            m.member.as_deref().unwrap_or("-"),
            m.alarm
                .as_deref()
                .map(|a| format!("ALARM: {a}"))
                .or_else(|| m.error.as_ref().map(|e| format!("delete failed: {e}")))
                .unwrap_or_default()
        );
    }
    Ok(())
}

/// `devstack node mock-provisioner`: a provisioner without a cloud, to try
/// provisioning (ADR-0051) on one machine. Each machine it "makes" is the
/// command `exec` (run with `DEVSTACK_ENROL_COMMAND`,
/// `DEVSTACK_ENROL_COORDINATOR`, `DEVSTACK_ENROL_FINGERPRINT`,
/// `DEVSTACK_ENROL_TOKEN`, `DEVSTACK_MACHINE`), or without it the line to
/// enrol one by hand, printed.
#[allow(clippy::too_many_arguments)]
pub fn mock_provisioner(
    listen: &str,
    address: &str,
    (cpus, memory_gib): (f64, f64),
    estimate: u64,
    refuse: Option<String>,
    fail_deletes: u32,
    token: Option<String>,
    exec: Option<String>,
) -> Result<()> {
    use devstack_node::provision::mock::{Behaviour, Mock};
    let listener =
        std::net::TcpListener::bind(listen).map_err(|e| anyhow!("--listen {listen}: {e}"))?;
    let hook = move |r: devstack_node::provision::MachineRequest, machine: String| {
        println!(
            "machine {machine} for request {} ({:.1} CPU, {}{}): enrol it with\n  {}",
            r.request,
            r.need.cpus,
            coordinator::gib(r.need.memory),
            r.repo
                .as_ref()
                .map_or(String::new(), |x| format!(", repository {x}")),
            r.enrol.command
        );
        let Some(cmd) = &exec else { return };
        let mut c = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", cmd]);
            c
        } else {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", cmd]);
            c
        };
        let started = c
            .env("DEVSTACK_ENROL_COMMAND", &r.enrol.command)
            .env("DEVSTACK_ENROL_COORDINATOR", &r.enrol.coordinator)
            .env("DEVSTACK_ENROL_FINGERPRINT", &r.enrol.fingerprint)
            .env("DEVSTACK_ENROL_TOKEN", &r.enrol.token)
            .env("DEVSTACK_MACHINE", &machine)
            .spawn();
        if let Err(e) = started {
            eprintln!("--exec {cmd}: {e}");
        }
    };
    let mock = Mock::serve(
        listener,
        Behaviour {
            size: devstack_node::share::Amount {
                cpus,
                memory: (memory_gib * f64::from(1u32 << 30)) as u64,
            },
            address: address.into(),
            estimate,
            refuse,
            fail_deletes,
            token,
            hook_after: Duration::from_millis(500),
        },
        Some(Box::new(hook)),
    );
    println!(
        "mock provisioner at {}: on the coordinator, `devstack node serve --listen <address:port> --provisioner {}`",
        mock.url, mock.url
    );
    let mut told = (0, 0, 0);
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let seen = mock.seen();
        for e in &seen.events[told.0..] {
            println!(
                "event {} of {}{}",
                e.kind,
                e.machine,
                e.lease.as_ref().map_or(String::new(), |l| format!(
                    ": {}/{} of {}",
                    l.repo, l.lane, l.member
                ))
            );
        }
        for d in &seen.deleted[told.1..] {
            println!("deleted machine {d}");
        }
        if seen.failed_deletes as usize > told.2 {
            println!(
                "failed a deletion on purpose ({} so far)",
                seen.failed_deletes
            );
        }
        told = (
            seen.events.len(),
            seen.deleted.len(),
            seen.failed_deletes as usize,
        );
    }
}

pub fn print_fingerprint(dir: Option<PathBuf>) -> Result<()> {
    let dir = coordinator_dir(dir);
    need_coordinator(&dir)?;
    println!("{}", fingerprint(&dir)?);
    Ok(())
}

pub(crate) fn ago(secs: u64) -> String {
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
    (memory_headroom, max_cpus, share_percent): (Option<f64>, Option<f64>, Option<u8>),
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
        || share_percent.is_some()
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
                share_percent,
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
            "{} ({}): allow {}; repositories {}; memory {}; CPU {}; offers {}% of what is free; network {}",
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
            p.share_percent.unwrap_or(50),
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
    enrol: Option<&str>,
) -> Result<()> {
    let local = Local::new(&member_dir(pool)?);
    // Asking again keeps the name it asked with.
    let name = match name {
        Some(n) => n.to_string(),
        None => local.membership()?.map_or_else(hostname, |m| m.name),
    };
    // A machine a provisioner made (ADR-0051 point 6): once, and again for
    // a minute while the coordinator has not heard of it from the
    // provisioner.
    if let Some(token) = enrol {
        let t = std::time::Instant::now();
        let m = loop {
            match local.enrol(coordinator, fingerprint, &name, token) {
                Err(devstack_node::member::MemberError::Status(409, why))
                    if t.elapsed() < Duration::from_secs(60) =>
                {
                    eprintln!("{why}");
                    std::thread::sleep(Duration::from_secs(2));
                }
                r => break r?,
            }
        };
        println!(
            "enrolled in the pool `{pool}` at {} as {} ({name}), a provisioned node; `devstack node agent --dedicated` runs its lanes",
            m.coordinator, m.id
        );
        return Ok(());
    }
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
/// `devstack pool default-on [ON]`: where this machine's lanes start
/// without `--on` (ADR-0025, amended 2026-10-21); shows it without `ON`.
pub fn default_on(on: Option<&str>) -> Result<()> {
    use devstack_node::member;
    let state = devstack_daemon::remote::state_dir();
    let Some(on) = on.map(str::trim).filter(|o| !o.is_empty()) else {
        match member::default_on(&state) {
            Some(o) => println!(
                "lanes start on `{o}` without --on (`devstack pool default-on local` keeps them here)"
            ),
            None => println!(
                "lanes start on this machine without --on (`devstack pool default-on pool` sends them to the pool)"
            ),
        }
        return Ok(());
    };
    if !member::is_here(on) {
        // A pool this machine belongs to, approved: else the start would fail.
        let (pool, _) = devstack_daemon::remote::resolve_on(on).map_err(|e| anyhow::anyhow!(e))?;
        let approved = devstack_daemon::remote::local_member(&pool)
            .membership()
            .ok()
            .flatten()
            .is_some_and(|m| m.state == devstack_node::store::State::Approved);
        if !approved {
            anyhow::bail!(
                "this machine is no approved member of the pool `{pool}`: `devstack join` first"
            );
        }
    }
    member::set_default_on(&state, Some(on))?;
    if member::is_here(on) {
        println!("lanes start on this machine without --on");
    } else {
        println!(
            "lanes start on `{on}` without --on: their worktree's files go there without asking (`devstack pool default-on local` takes it back)"
        );
    }
    Ok(())
}

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

/// `devstack pool provisioned [on|off]`: whether this machine's lanes may
/// run on nodes a provisioner made (ADR-0051 point 10).
pub fn provisioned(pool: &str, state: Option<&str>) -> Result<()> {
    let local = Local::new(&member_dir(pool)?);
    if let Some(s) = state {
        local.set_provisioned(on_off(s)?)?;
    }
    let me = local.me()?;
    if me.no_provisioned {
        println!(
            "pool `{pool}`: this machine's lanes stay off provisioned nodes (`devstack pool provisioned on` lets them go there)"
        );
    } else {
        println!(
            "pool `{pool}`: this machine's lanes may run on provisioned nodes; whoever runs the pool's provisioner can read them there (`devstack pool provisioned off` keeps them off)"
        );
    }
    if me.provision {
        println!(
            "an admin allowed it to cause provisioning: a lane no node can take makes a machine"
        );
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
            free: None,
            on_update: Some(std::sync::Arc::new(crate::update::for_node)),
        },
        addr,
    )?;
    Ok(())
}

/// A systemd unit that runs `devstack node agent` with these arguments, as
/// the user who prints it, and starts it again whenever it ends: after an
/// update an admin ordered the agent runs the new release (ADR-0039).
pub fn agent_unit(
    pool: &str,
    listen: &str,
    api: Option<&str>,
    max_cpus: Option<f64>,
    max_memory_gib: Option<f64>,
    dedicated: bool,
) -> Result<String> {
    let exe = std::env::current_exe()?;
    let mut args = vec![
        "node".to_string(),
        "agent".into(),
        "--listen".into(),
        listen.into(),
        "--pool".into(),
        pool.into(),
    ];
    if let Some(a) = api {
        args.extend(["--api".into(), a.into()]);
    }
    if let Some(c) = max_cpus {
        args.extend(["--max-cpus".into(), c.to_string()]);
    }
    if let Some(m) = max_memory_gib {
        args.extend(["--max-memory-gib".into(), m.to_string()]);
    }
    if dedicated {
        args.push("--dedicated".into());
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
    let home = std::env::var("HOME").unwrap_or_default();
    let name = if pool == devstack_node::member::DEFAULT_POOL {
        "devstack-node".to_string()
    } else {
        format!("devstack-node-{pool}")
    };
    Ok(format!(
        "# /etc/systemd/system/{name}.service
# sudo systemctl daemon-reload && sudo systemctl enable --now {name}
[Unit]
Description=devstack node agent (pool {pool})
After=network-online.target docker.service
Wants=network-online.target

[Service]
User={user}
Environment={home}
ExecStart={exe} {args}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
",
        home = systemd_word(&format!("HOME={home}")),
        exe = systemd_word(&exe.to_string_lossy()),
        args = args
            .iter()
            .map(|a| systemd_word(a))
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

/// One word of a systemd unit's `ExecStart=` or `Environment=`: as it is
/// when it is plain, else in double quotes, `\` and `"` escaped, `%` and `$`
/// doubled (systemd expands both, quoted or not).
fn systemd_word(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._:@,=+-".contains(c));
    if plain {
        return s.to_string();
    }
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str(r#"\""#),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
        need: devstack_node::share::Need {
            cpus: 0.5,
            memory: 1 << 30,
            ..Default::default()
        },
        need_from: Some("a default (`devstack node run-lane`)".into()),
        floor: devstack_daemon::remote::floor_of(&stack.lane_need(None)),
        profile: None,
        devstack: Some(devstack_node::version::Release::this()),
        warm: false,
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
        for f in &s.failed {
            eprintln!("not written on the node: {f}");
        }
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
                None,
                false,
                false,
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
        // Its services start once its containers are up, which the node
        // brings up after the daemon answered (#164): until one of them
        // moves, every service is still `stopped`.
        let mut moved = false;
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
            let failed: Vec<String> = v["infra"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|g| {
                    Some(format!(
                        "{}: {}",
                        g["group"].as_str()?,
                        g["error"].as_str()?
                    ))
                })
                .collect();
            if !failed.is_empty() {
                println!("compose group failed: {}", failed.join("; "));
                break;
            }
            // A build while the containers start is no start: a service
            // built ahead goes back to `stopped` until its turn.
            moved |= states
                .iter()
                .any(|s| !s.ends_with("=stopped") && !s.ends_with("=building"));
            let settled = moved
                && states.iter().all(|s| {
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

/// `devstack pool upstreams [list|add|remove]`: the MCP upstreams the pool
/// names for its members (ADR-0069 option B).
pub fn upstreams(pool: &str, action: Option<crate::PoolUpstreamsAction>) -> Result<()> {
    use crate::PoolUpstreamsAction as A;
    use devstack_node::upstreams::{PoolAuth, PoolUpstream};
    let local = Local::new(&member_dir(pool)?);
    let list = match action.unwrap_or(A::List) {
        A::List => local.upstreams()?,
        A::Add {
            name,
            url,
            prefix,
            allow,
            login,
            client_id,
        } => {
            let auth = match login.as_str() {
                "oauth" => PoolAuth::Oauth {
                    client_id,
                    scopes: Vec::new(),
                },
                "token" | "none" if client_id.is_some() => {
                    bail!("`--client-id` is for `--login oauth`")
                }
                "token" => PoolAuth::Token,
                "none" => PoolAuth::None,
                o => bail!("`{o}`: `oauth`, `token` or `none`"),
            };
            let u = PoolUpstream {
                name,
                url,
                prefix,
                allow: allow.into_iter().filter(|t| !t.trim().is_empty()).collect(),
                auth,
            };
            u.check().map_err(|e| anyhow!(e))?;
            let list = local.admin_upstream_put(&u)?;
            println!(
                "pool `{pool}` names `{}`: each member's hub offers it in the dashboard, and uses it once its user accepts it",
                u.name
            );
            list
        }
        A::Remove { name } => {
            local.admin_upstream_remove(&name)?;
            println!("pool `{pool}` no longer names `{name}`: members' hubs drop it and its login");
            local.upstreams()?
        }
    };
    if list.is_empty() {
        println!("pool `{pool}` names no MCP upstreams");
        return Ok(());
    }
    println!("MCP upstreams of pool `{pool}`:");
    for u in &list {
        let login = match &u.auth {
            PoolAuth::None => "no login".to_string(),
            PoolAuth::Token => "a token each member gives".to_string(),
            PoolAuth::Oauth {
                client_id: Some(c), ..
            } => format!("OAuth (client id {c})"),
            PoolAuth::Oauth { .. } => "OAuth".to_string(),
        };
        let tools = if u.allow.is_empty() {
            "all tools".to_string()
        } else {
            u.allow.join(", ")
        };
        println!(
            "  {:<16} {}  {}__…  {login}; {tools}",
            u.name,
            u.url,
            u.prefix()
        );
    }
    Ok(())
}

/// `devstack pool skills [list|add|remove|refresh|status|sync]`: the skill
/// sets the pool hands out (ADR-0074).
pub fn skills(pool: &str, action: Option<crate::PoolSkillsAction>) -> Result<()> {
    use crate::PoolSkillsAction as A;
    use devstack_node::skills::{SkillSet, Source};
    let local = Local::new(&member_dir(pool)?);
    match action.unwrap_or(A::List) {
        A::List => {}
        A::Add {
            name,
            github,
            reference,
            path,
            folder,
            to,
        } => {
            let source = match (github, folder) {
                (Some(repo), None) => Source::Github {
                    repo,
                    reference,
                    path,
                },
                (None, Some(path)) => Source::Folder { path },
                _ => bail!("name a source: `--github owner/name` or `--folder <path>`"),
            };
            let set = SkillSet {
                name,
                source,
                to: to.into_iter().filter(|t| !t.trim().is_empty()).collect(),
            };
            set.check().map_err(|e| anyhow!(e))?;
            local.admin_skill_put(&set)?;
            println!(
                "pool `{pool}` hands out `{}` ({}); the coordinator fetches it now, and each member's hub offers it in the dashboard",
                set.name,
                set.source.label()
            );
        }
        A::Remove { name } => {
            local.admin_skill_remove(&name)?;
            println!("pool `{pool}` no longer hands out `{name}`: members' hubs remove its skills");
        }
        A::Refresh { set } => {
            local.admin_skills_refresh(set.as_deref())?;
            println!(
                "the coordinator fetches {} again",
                set.map_or("every set".into(), |s| format!("`{s}`"))
            );
            return Ok(());
        }
        A::Status => return skills_here(None),
        A::Sync => return skills_here(Some("/hub/skills/sync")),
    }
    match local.admin_skills() {
        Ok(v) => {
            if v.sets.is_empty() {
                println!("pool `{pool}` hands out no skill sets");
            }
            for s in &v.sets {
                let m = &s.manifest;
                println!(
                    "{:<16} {}  to {}",
                    s.set.name,
                    s.set.source.label(),
                    s.set.to.join(",")
                );
                print_manifest(m);
            }
            if !v.github_token {
                println!(
                    "(no DEVSTACK_SKILLS_GITHUB_TOKEN on the coordinator: public repositories only)"
                );
            }
            if !v.refresh_signed {
                println!(
                    "(no DEVSTACK_SKILLS_REFRESH_SECRET on the coordinator: a push is not told, refresh by hand)"
                );
            }
        }
        Err(devstack_node::member::MemberError::Status(403, _)) => {
            let sets = local.skills()?;
            if sets.is_empty() {
                println!("pool `{pool}` gives this machine no skill sets");
            }
            for m in &sets {
                println!("{:<16} {}", m.name, m.source);
                print_manifest(m);
            }
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn print_manifest(m: &devstack_node::skills::SetManifest) {
    if let Some(e) = &m.error {
        println!("  last fetch failed: {e}");
    }
    for k in &m.skills {
        println!("  {:<24} {}", k.name, k.description);
    }
    for k in &m.skipped {
        println!("  left out: {k}");
    }
}

/// This machine's state from the hub, after `post` (a path) if given.
fn skills_here(post: Option<&str>) -> Result<()> {
    let port = devstack_hub::ensure_running().map_err(|e| anyhow!("the hub: {e}"))?;
    let token = devstack_daemon::token::load()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("no user token: start the hub once (`devstack ui`)"))?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let auth = format!("Bearer {token}");
    let mut r = match post {
        Some(p) => agent
            .post(&format!("http://127.0.0.1:{port}{p}"))
            .header("Authorization", &auth)
            .header("X-Devstack-Client", "cli")
            .send("")?,
        None => agent
            .get(&format!("http://127.0.0.1:{port}/hub/skills"))
            .header("Authorization", &auth)
            .header("X-Devstack-Client", "cli")
            .call()?,
    };
    let text = r.body_mut().read_to_string()?;
    if !r.status().is_success() {
        bail!("the hub answered {}: {text}", r.status());
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    println!("agents (installed into when on):");
    for a in v["agents"].as_array().into_iter().flatten() {
        println!(
            "  [{}] {:<20} {}",
            if a["enabled"].as_bool() == Some(true) {
                "x"
            } else {
                " "
            },
            a["name"].as_str().unwrap_or(""),
            a["dir"].as_str().unwrap_or("")
        );
    }
    for s in v["sets"].as_array().into_iter().flatten() {
        println!(
            "{}:{}  {}  {}{}",
            s["pool"].as_str().unwrap_or(""),
            s["set"].as_str().unwrap_or(""),
            s["source"].as_str().unwrap_or(""),
            s["state"].as_str().unwrap_or(""),
            if s["auto"].as_bool() == Some(true) {
                ", auto-update"
            } else {
                ""
            }
        );
        for k in s["skills"].as_array().into_iter().flatten() {
            let state = if k["enabled"].as_bool() == Some(false) {
                "off"
            } else if k["pending"].as_bool() == Some(true) {
                "update waits"
            } else if k["installed"].as_bool() == Some(true) {
                "installed"
            } else {
                ""
            };
            let group = k["group"]
                .as_str()
                .map(|g| format!("  [{g}]"))
                .unwrap_or_default();
            println!("  {:<24} {state}{group}", k["name"].as_str().unwrap_or(""));
        }
    }
    for c in v["conflicts"].as_array().into_iter().flatten() {
        println!(
            "conflict: {} in {}: {}",
            c["name"].as_str().unwrap_or(""),
            c["dir"].as_str().unwrap_or(""),
            c["reason"].as_str().unwrap_or("")
        );
    }
    for e in v["errors"].as_array().into_iter().flatten() {
        println!("error: {}", e.as_str().unwrap_or(""));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::systemd_word;

    #[test]
    fn a_unit_quotes_what_has_spaces_or_specifiers() {
        assert_eq!(
            systemd_word("/usr/local/bin/devstack"),
            "/usr/local/bin/devstack"
        );
        assert_eq!(
            systemd_word("/home/a b/.local/bin/devstack"),
            r#""/home/a b/.local/bin/devstack""#
        );
        assert_eq!(systemd_word("HOME=/home/x%y"), r#""HOME=/home/x%%y""#);
        assert_eq!(systemd_word(r#"a"b$c\d"#), r#""a\"b$$c\\d""#);
        assert_eq!(systemd_word(""), r#""""#);
        assert_eq!(systemd_word("0.0.0.0:7480"), "0.0.0.0:7480");
    }
}
