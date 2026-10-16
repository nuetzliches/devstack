//! `devstack`: one binary for daemon, CLI and MCP server (ADR-0005, ADR-0007).
//! Phase 1 starts with the commands that need no daemon.

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

mod buildcmd;
mod client;
mod doctor;
mod envcmd;
mod hubcmd;
mod init;
mod integration;
mod mcp;
mod poolcmd;
mod selfcmd;
mod skills;
mod topcmd;
mod update;

#[derive(Parser)]
#[command(version, about = "Local development orchestrator")]
struct Cli {
    /// Stack file.
    #[arg(long, short = 'f', global = true, default_value = "devstack.toml")]
    file: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate the stack file and report every problem at once.
    Check {
        /// Also validate every Compose group on its own (`docker compose config -q`, ADR-0015).
        #[arg(long)]
        compose: bool,
    },
    /// Sync the lane's snapshot and build a service into an artifact (ADR-0002).
    Build {
        service: String,
        /// Lane to build from; `main` is the repository itself.
        #[arg(long, default_value = "main")]
        lane: String,
    },
    /// Start the stack: daemon (if needed), infrastructure, services in order.
    /// In a worktree: its lane, with its own infrastructure (ADR-0030).
    Up {
        /// In a worktree: the lane's services run their background workers.
        #[arg(long)]
        workers: bool,
        /// In a worktree: start the lane although its memory check says it
        /// does not fit.
        #[arg(long)]
        force: bool,
        /// In a worktree: run its lane on this node of the pool (its name or
        /// member id, see `devstack pool`), or `pool` to let the coordinator
        /// choose, instead of on this machine (ADR-0025). Everything of the
        /// lane runs there; the worktree stays.
        #[arg(long, value_name = "NODE")]
        on: Option<String>,
    },
    /// Stop the services and the daemon; `--infra` also removes the containers.
    Down {
        #[arg(long)]
        infra: bool,
    },
    /// State of services and infrastructure.
    Status,
    /// Start a service, or every service of a group.
    Start {
        /// A service or a group of the stack file.
        service: String,
    },
    /// Stop a service, or every service of a group.
    Stop {
        /// A service or a group of the stack file.
        service: String,
    },
    /// Rebuild and restart a service; for a group every service (built at the same time) and its containers.
    Restart {
        /// A service or a group of the stack file.
        service: String,
        /// Build again even if an artifact for the same inputs exists (an
        /// input devstack does not see changed: the SDK, a file outside the
        /// repository). The build gets the id `<inputs>-<n>`.
        #[arg(long)]
        rebuild: bool,
    },
    /// Run a built service with its toolchain's watcher from the source tree
    /// (`hot_reload` in the stack file), or back to builds (plan §3.4a).
    #[command(name = "hot-reload")]
    HotReload {
        service: String,
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Take over a candidate that waits for promotion (a schema change, ADR-0013).
    Promote {
        /// The service whose held candidate takes over.
        #[arg(required_unless_present = "lane", conflicts_with = "lane")]
        service: Option<String>,
        /// Instead: this lane's services answer on the main ports (`main`
        /// gives them back to the main working tree), plan §3.5.
        #[arg(long)]
        lane: Option<String>,
    },
    /// Lanes: the repository's other working trees (`git worktree`), running
    /// beside the main one on their own ports with their own containers
    /// (plan §3.5, ADR-0030), here or on a node of the pool (ADR-0025). Every
    /// worktree is a lane (`git worktree add` makes one); `devstack up` in
    /// it starts it, `devstack up --on <node>` there.
    Lane {
        #[command(subcommand)]
        action: LaneAction,
    },
    /// Go back to the build that ran before the last replacement (no build, no jobs).
    Rollback { service: String },
    /// Console output of a service.
    Logs {
        /// The service, or a Compose container by its service name; not with
        /// --daemon / --hub.
        service: Option<String>,
        #[arg(long, short = 'n', default_value_t = 200)]
        tail: usize,
        #[arg(long)]
        follow: bool,
        /// devstack's own log for this stack: every decision with its reason (JSON lines).
        #[arg(long, conflicts_with_all = ["service", "hub"])]
        daemon: bool,
        /// The dashboard hub's log.
        #[arg(long, conflicts_with = "service")]
        hub: bool,
        /// Only lines containing this (case-insensitive). Without a service:
        /// every service's current log, each line prefixed with its service.
        #[arg(long)]
        grep: Option<String>,
        /// The logs of this build: its build log and every instance that ran
        /// it (a prefix of the id is enough).
        #[arg(long, conflicts_with_all = ["follow", "daemon", "hub"])]
        build: Option<String>,
        /// Every instance's log written to within this time (`30m`, `2h`),
        /// not only the current one. Lines carry no time of their own.
        #[arg(long, conflicts_with_all = ["follow", "daemon", "hub"])]
        since: Option<String>,
        /// The service's kept logs (instances and builds), newest first.
        #[arg(long, requires = "service", conflicts_with_all = ["follow", "grep", "build", "since", "log"])]
        list: bool,
        /// One kept log by its file name (see --list).
        #[arg(long, requires = "service", conflicts_with_all = ["follow", "build", "since"])]
        log: Option<String>,
        /// With --daemon / --hub: the raw JSON lines instead of readable ones.
        #[arg(long)]
        json: bool,
    },
    /// Serve a directory as a static site (what a `kind = "static"` service
    /// runs; started by the daemon).
    #[command(name = "serve-static", hide = true)]
    ServeStatic {
        dir: PathBuf,
        #[arg(long)]
        port: u16,
        /// Without the single-page-app fallback to index.html.
        #[arg(long)]
        no_spa: bool,
    },
    /// The builds of a service, newest first: which went live, failed, were
    /// replaced by a newer change or wait for promote.
    Builds {
        service: String,
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
    },
    /// CPU, memory and disk per service, container, group and lane (plan §12).
    Top {
        /// Refresh every two seconds.
        #[arg(long, short = 'w')]
        watch: bool,
    },
    /// Run a command from the stack file (e.g. a database reset).
    Run {
        command: String,
        /// Confirm without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Internal: the daemon in the foreground (`devstack up` starts it in the background).
    #[command(hide = true)]
    Daemon,
    /// Show the environment a service gets, with the origin of every variable
    /// (ADR-0014), or export it as a dotenv file for use without devstack (ADR-0015).
    Env {
        service: String,
        #[arg(long, value_enum)]
        export: Option<ExportFormat>,
        /// Write the export to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// MCP server on stdio for coding agents; a client of the daemon like the CLI.
    Mcp,
    /// Open the dashboard: starts the hub if needed and pairs the browser (ADR-0021).
    Ui {
        /// Print the one-time pairing link instead of opening a browser.
        #[arg(long)]
        print: bool,
        /// The browser to pair: `chrome`, `edge`, `firefox`, `brave`, or the
        /// path of a browser executable. Default: `DEVSTACK_BROWSER`, then
        /// `BROWSER`, then the system's default browser.
        #[arg(long, value_name = "NAME|PATH")]
        browser: Option<String>,
    },
    /// The hub that serves the dashboard for every stack; without an action it
    /// runs in the foreground (normally started by `devstack up` / `devstack ui`).
    Hub {
        #[command(subcommand)]
        action: Option<HubAction>,
    },
    /// The coordinator of a pool of machines that run lanes (ADR-0031), on an
    /// always-on host.
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// Ask a pool's coordinator to admit this machine, and wait for the
    /// approval (ADR-0031). Asking again is the same request.
    Join {
        /// The coordinator, `https://host:port`.
        coordinator: String,
        /// The SHA-256 of the coordinator's CA (`devstack node fingerprint` on it).
        #[arg(long)]
        fingerprint: String,
        /// How the machine shows in the pool; default its host name.
        #[arg(long)]
        name: Option<String>,
        /// Ask and return; `devstack join` again picks up the approval.
        #[arg(long)]
        no_wait: bool,
        /// The name this machine gives the pool (ADR-0034): a machine may
        /// belong to several, e.g. a customer's beside its team's.
        #[arg(long, default_value = "default")]
        pool: String,
        /// Agree to send the pool's collector telemetry, if it has one
        /// (ADR-0037); `devstack pool telemetry off` takes it back.
        #[arg(long)]
        telemetry: bool,
    },
    /// The pools this machine belongs to: their members, online or not, and
    /// what each shares.
    Pool {
        /// Only this pool.
        #[arg(long)]
        pool: Option<String>,
        #[command(subcommand)]
        action: Option<PoolAction>,
    },
    /// Print the JSON Schema of the stack file (for editors).
    Schema,
    /// Is this machine ready for the stack? Docker, Compose, the tools the stack
    /// file runs, ports, the hub, PATH. Read-only; exits 1 when something fails.
    Doctor,
    /// Write a first devstack.toml from what the repository shows (Compose files,
    /// .NET, Node, Go, Python and Spring Boot apps); every guess is a TODO.
    Init {
        /// The repository; default the current directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print the draft instead of writing it.
        #[arg(long)]
        print: bool,
        /// A summary as JSON (for the hub).
        #[arg(long)]
        json: bool,
    },
    /// Install the latest release (or `--version`) with the official installer
    /// and move the hub onto it. Running stacks switch at their next restart.
    Update {
        /// Only say whether a newer release exists.
        #[arg(long)]
        check: bool,
        /// A specific release, e.g. 0.4.0.
        #[arg(long)]
        version: Option<String>,
    },
    /// devstack itself on this machine.
    #[command(name = "self")]
    Myself {
        #[command(subcommand)]
        action: SelfAction,
    },
    /// Agent skills for this repository, generated from the stack file.
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Internal: what the system runs for a `devstack://` link (only `devstack://hub/start`).
    #[command(hide = true)]
    OpenUrl { url: String },
    /// Open a service of a lane someone in the pool shares with you
    /// (ADR-0035): a forward on this machine, then the browser.
    Open {
        /// The lane's name (or its lease, see `devstack pool shared`).
        lane: String,
        /// The service; may be left out when the lane shares one.
        service: Option<String>,
        /// The pool (ADR-0034); default the default one.
        #[arg(long)]
        pool: Option<String>,
    },
    /// Internal (Linux): the shim between daemon and service (ADR-0009).
    #[command(hide = true)]
    Shim {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PoolAction {
    /// Whether this machine sends the pool's collector telemetry (ADR-0037):
    /// `on`, `off`, or nothing to show it and what is sent.
    Telemetry { state: Option<String> },
    /// The lanes others share with this machine (ADR-0035).
    Shared,
}

#[derive(Subcommand)]
enum LaneAction {
    /// The lanes (the repository's worktrees), their branches and ports.
    List,
    /// Stop a lane: its services stop and it is no longer started (its
    /// builds stay). The worktree is git's: devstack never creates or removes
    /// one.
    Stop { name: String },
    /// Move a started lane to another node of the pool, or back to this
    /// machine (ADR-0025): a cold start there (its data comes from the seed
    /// jobs again), which takes over once it is ready.
    Move {
        name: String,
        /// A node (its name or member id, see `devstack pool`), or `local`.
        #[arg(long)]
        to: String,
    },
    /// Share a lane on a node with others in its pool (ADR-0035): they see
    /// it in their dashboard and open its services. Without `--with`: with
    /// nobody again.
    Share {
        name: String,
        /// A member id or a tag (`team:qa`), or `*` for every member; repeat.
        #[arg(long)]
        with: Vec<String>,
    },
}

#[derive(Subcommand)]
enum NodeAction {
    /// Create the coordinator: its certificate authority and a server
    /// certificate for the addresses members reach it at.
    Init {
        /// An IP address or host name members reach the coordinator at (repeat
        /// for more).
        #[arg(long = "address", required = true)]
        addresses: Vec<String>,
        /// Its directory; default `DEVSTACK_COORDINATOR_DIR`, else devstack's
        /// per-user state.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Run the coordinator in the foreground.
    Serve {
        /// Address and port to listen on, e.g. the host's address in the
        /// pool's private network.
        #[arg(long)]
        listen: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Try a lane on a node without the daemon: lease, sync this repository,
    /// start it there, show its services until they are up (M2b; `devstack
    /// up --on` replaces it).
    #[command(hide = true)]
    RunLane {
        /// The node: its name or member id.
        #[arg(long)]
        on: String,
        #[arg(long, default_value = "try")]
        lane: String,
        /// Leave it running (renew the lease yourself); default: release it.
        #[arg(long)]
        keep: bool,
    },
    /// Run other members' lanes on this machine (ADR-0025): admit leases,
    /// keep their source mirrors, report to the coordinator. The machine
    /// must have joined the pool (`devstack join`).
    Agent {
        /// Address and port to listen on.
        #[arg(long)]
        listen: String,
        /// Where requesters reach this node: `https://host:port`; default
        /// from `--listen`.
        #[arg(long)]
        api: Option<String>,
        /// The most CPU cores it offers (default: half of what is free).
        #[arg(long)]
        max_cpus: Option<f64>,
        /// The most memory it offers, in GiB.
        #[arg(long)]
        max_memory_gib: Option<f64>,
        /// A server its owner never reclaims (ADR-0026): the pool shows it as
        /// one; a developer's machine leaves it out.
        #[arg(long)]
        dedicated: bool,
        /// The pool it runs lanes for (ADR-0034); one agent per pool.
        #[arg(long, default_value = "default")]
        pool: String,
    },
    /// The CA's fingerprint, which a member passes to `devstack join`.
    Fingerprint {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// The members: who asked, who is approved.
    Members {
        #[command(subcommand)]
        action: Option<MembersAction>,
        #[arg(long, global = true)]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum MembersAction {
    /// Every member and its state (the default).
    List,
    /// Admit a member (by id, a unique prefix of it, or its name).
    Approve {
        member: String,
        /// As an admin, who manages the pool from the dashboard (ADR-0032).
        #[arg(long)]
        admin: bool,
    },
    /// Remove a member, or reject its request: its certificate is refused
    /// from now on.
    Remove { member: String },
    /// Change an approved member's role: `member` or `admin` (the last admin
    /// stays one).
    Role { member: String, role: String },
    /// Set a member's tags (ADR-0033), replacing its tags: `team:backend`,
    /// `customer:acme`; none clears them.
    Tags { member: String, tags: Vec<String> },
    /// What a node lets run on it (ADR-0033); without options it shows it.
    Policy {
        node: String,
        /// Who may start lanes there: a member id or a tag; repeat. None: all.
        #[arg(long)]
        allow: Vec<String>,
        /// A repository (`[stack] name`) it runs; repeat. None: any.
        #[arg(long)]
        repo: Vec<String>,
        /// A lane's memory limit as its expected need times this.
        #[arg(long)]
        memory_headroom: Option<f64>,
        /// The most CPU cores one lane may use.
        #[arg(long)]
        max_cpus: Option<f64>,
        /// `lan` (the internet and the local network), `internet` (not the
        /// local network) or `none` (only the lane's own containers).
        #[arg(long)]
        network: Option<String>,
        /// Lift the node's policy.
        #[arg(long)]
        clear: bool,
    },
}

#[derive(Subcommand)]
enum HubAction {
    /// Is the hub running, and where.
    Status,
    /// Stop the hub (stacks keep running).
    Stop,
    /// Start the hub in the background unless it runs.
    Start,
    /// Start the hub at logon and let the dashboard start it (`devstack://` link).
    /// `devstack up` does this once by itself.
    Install {
        /// Register the link only, no start at logon.
        #[arg(long)]
        no_autostart: bool,
    },
    /// Remove both again; `devstack up` then leaves them alone.
    Uninstall,
}

#[derive(Subcommand)]
enum SelfAction {
    /// Remove devstack for this user: hub, its start at logon and link, the
    /// per-user state, the PATH entry and the installed binary. Repositories keep
    /// their .devstack/ directories. Refuses while a stack runs.
    Uninstall {
        /// Do not ask.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum SkillsAction {
    /// Write the skills `devstack` (run the stack) and `devstack-config` (edit the
    /// stack file), filled in from this stack file. Again to refresh them.
    Install {
        /// Target directory; default `.claude/skills` next to the stack file.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Also replace a file of the same name the project wrote itself.
        #[arg(long)]
        force: bool,
    },
    /// Do the installed skills match this devstack and this stack file?
    Check,
}

#[derive(Clone, Copy, ValueEnum)]
enum ExportFormat {
    Dotenv,
}

fn load(file: &Path) -> devstack_config::Stack {
    match devstack_config::load(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// The repository root: the directory of the stack file.
fn repo_of(file: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(file)?;
    Ok(abs.parent().map(Path::to_path_buf).unwrap_or(abs))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Check { compose } => {
            let stack = load(&cli.file);
            println!(
                "{}: ok ({} compose group(s), {} service(s), {} command(s))",
                cli.file.display(),
                stack.compose.len(),
                stack.services.len(),
                stack.commands.len()
            );
            if let Ok(text) = std::fs::read_to_string(&cli.file) {
                for w in devstack_config::compat::warnings(&text) {
                    println!("warning: {w}");
                }
            }
            if compose {
                let repo = repo_of(&cli.file)?;
                let docker = devstack_compose::Docker::default();
                let mut failed = false;
                for (name, g) in &stack.compose {
                    let group = devstack_compose::Group::from_config(name, g, &repo);
                    match docker.validate(&group) {
                        Ok(()) => println!("compose.{name}: ok"),
                        Err(e) => {
                            failed = true;
                            eprintln!("compose.{name}: {e}");
                        }
                    }
                }
                if failed {
                    std::process::exit(1);
                }
                // Reference-only links (ADR-0014): what the deployment sets that
                // the stack file neither sets nor ignores. A notice, not an error.
                for (name, svc) in &stack.services {
                    let Some(r) = svc.compose.as_ref().filter(|r| !r.inherit) else {
                        continue;
                    };
                    let group = devstack_compose::Group::from_config(
                        &r.group,
                        &stack.compose[&r.group],
                        &repo,
                    );
                    let model = docker.config(&group)?;
                    let Some(m) = model.services.get(&r.service) else {
                        eprintln!(
                            "services.{name}: compose.{}.{} not found",
                            r.group, r.service
                        );
                        std::process::exit(1);
                    };
                    let drift = devstack_env::drift(svc, m.environment.keys().map(String::as_str));
                    if drift.is_empty() {
                        println!(
                            "services.{name}: no drift against compose.{}.{}",
                            r.group, r.service
                        );
                    } else {
                        println!(
                            "services.{name}: {} variable(s) of compose.{}.{} neither set nor ignored here:",
                            drift.len(),
                            r.group,
                            r.service
                        );
                        for k in drift {
                            println!("  {k}");
                        }
                    }
                }
            }
            Ok(())
        }
        Cmd::Up { workers, force, on } => client::up(&cli.file, workers, force, on.as_deref()),
        Cmd::Down { infra } => client::down(&cli.file, infra),
        Cmd::Status => {
            let c = client::need(&client::state_dir(&cli.file)?)?;
            client::print_status_for(&c.stack()?, &cli.file);
            Ok(())
        }
        Cmd::Start { service } => client::service_action(&cli.file, &service, "start"),
        Cmd::Stop { service } => client::service_action(&cli.file, &service, "stop"),
        Cmd::Restart { service, rebuild } => client::service_action(
            &cli.file,
            &service,
            if rebuild { "rebuild" } else { "restart" },
        ),
        Cmd::HotReload { service, state } => {
            client::service_action(&cli.file, &service, &format!("hot-reload-{state}"))
        }
        Cmd::Promote { service, lane } => match lane {
            Some(l) => client::promote_lane(&cli.file, &l),
            None => client::service_action(&cli.file, &service.unwrap_or_default(), "promote"),
        },
        Cmd::Lane { action } => match action {
            LaneAction::List => client::lane_list(&cli.file),
            LaneAction::Stop { name } => client::lane_stop(&cli.file, &name),
            LaneAction::Move { name, to } => client::lane_move(&cli.file, &name, &to),
            LaneAction::Share { name, with } => client::lane_share(&cli.file, &name, &with),
        },
        Cmd::Rollback { service } => client::service_action(&cli.file, &service, "rollback"),
        Cmd::Logs {
            service,
            tail,
            follow,
            daemon,
            hub,
            grep,
            build,
            since,
            list,
            log,
            json,
        } => {
            if list {
                client::log_files(&cli.file, service.as_deref().unwrap_or_default())
            } else if build.is_some() || since.is_some() || log.is_some() {
                client::logs_kept(
                    &cli.file,
                    service.as_deref(),
                    client::LogFilter {
                        tail,
                        grep: grep.as_deref(),
                        build: build.as_deref(),
                        since: since.as_deref(),
                        file: log.as_deref(),
                    },
                )
            } else if daemon || hub {
                let path = if hub {
                    devstack_hub::log_path()
                } else {
                    client::state_dir(&cli.file)?.join("daemon.log")
                };
                client::diag_log(&path, tail, grep.as_deref(), follow, json)
            } else if let Some(service) = service {
                match grep.as_deref() {
                    Some(g) => client::logs_grep(&cli.file, Some(&service), g, tail),
                    None => client::logs(&cli.file, &service, tail, follow),
                }
            } else if let Some(g) = grep.as_deref() {
                client::logs_grep(&cli.file, None, g, tail)
            } else {
                Err(anyhow::anyhow!(
                    "name a service, search all of them with --grep, or --daemon / --hub for devstack's own log"
                ))
            }
        }
        Cmd::Mcp => mcp::serve(&cli.file),
        Cmd::Ui { print, browser } => hubcmd::ui(print, browser.as_deref()),
        Cmd::Hub { action: None } => devstack_hub::serve().map_err(|e| anyhow::anyhow!(e)),
        Cmd::Hub {
            action: Some(HubAction::Status),
        } => {
            println!("{}", hubcmd::status_text());
            Ok(())
        }
        Cmd::Hub {
            action: Some(HubAction::Stop),
        } => hubcmd::stop(),
        Cmd::Hub {
            action: Some(HubAction::Start),
        } => {
            let port = devstack_hub::ensure_running().map_err(|e| anyhow::anyhow!(e))?;
            println!("hub running: http://127.0.0.1:{port}");
            Ok(())
        }
        Cmd::Hub {
            action: Some(HubAction::Install { no_autostart }),
        } => {
            integration::install(!no_autostart)?;
            println!("{}", integration::status_text());
            Ok(())
        }
        Cmd::Hub {
            action: Some(HubAction::Uninstall),
        } => {
            integration::uninstall()?;
            println!("{}", integration::status_text());
            Ok(())
        }
        Cmd::Node { action } => match action {
            NodeAction::Init { addresses, dir } => poolcmd::init(dir, &addresses),
            NodeAction::Serve { listen, dir } => poolcmd::serve(dir, &listen),
            NodeAction::Fingerprint { dir } => poolcmd::print_fingerprint(dir),
            NodeAction::RunLane { on, lane, keep } => {
                poolcmd::run_lane(&cli.file, &on, &lane, keep)
            }
            NodeAction::Agent {
                listen,
                api,
                max_cpus,
                max_memory_gib,
                dedicated,
                pool,
            } => poolcmd::agent(&pool, &listen, api, max_cpus, max_memory_gib, dedicated),
            NodeAction::Members { action, dir } => match action.unwrap_or(MembersAction::List) {
                MembersAction::List => poolcmd::members_list(dir),
                MembersAction::Approve { member, admin } => {
                    poolcmd::members_approve(dir, &member, admin)
                }
                MembersAction::Role { member, role } => poolcmd::members_role(dir, &member, &role),
                MembersAction::Tags { member, tags } => poolcmd::members_tags(dir, &member, &tags),
                MembersAction::Policy {
                    node,
                    allow,
                    repo,
                    memory_headroom,
                    max_cpus,
                    network,
                    clear,
                } => poolcmd::members_policy(
                    dir,
                    &node,
                    allow,
                    repo,
                    (memory_headroom, max_cpus),
                    network,
                    clear,
                ),
                MembersAction::Remove { member } => poolcmd::members_remove(dir, &member),
            },
        },
        Cmd::Join {
            coordinator,
            fingerprint,
            name,
            no_wait,
            pool,
            telemetry,
        } => {
            if telemetry {
                poolcmd::telemetry(&pool, Some("on"))?;
            }
            poolcmd::join(&pool, &coordinator, &fingerprint, name.as_deref(), !no_wait)
        }
        Cmd::Pool { pool, action } => match action {
            None => poolcmd::pool(pool.as_deref()),
            Some(PoolAction::Shared) => hubcmd::shared(pool.as_deref()),
            Some(PoolAction::Telemetry { state }) => poolcmd::telemetry(
                pool.as_deref()
                    .unwrap_or(devstack_node::member::DEFAULT_POOL),
                state.as_deref(),
            ),
        },
        Cmd::OpenUrl { url } => integration::open_url(&url),
        Cmd::Open {
            lane,
            service,
            pool,
        } => hubcmd::open_shared(&lane, service.as_deref(), pool.as_deref()),
        Cmd::Init { dir, print, json } => {
            let repo = match dir {
                Some(d) => d,
                None => std::env::current_dir()?,
            };
            init::run(&repo, print, json)
        }
        Cmd::Update { check, version } => update::run(check, version.as_deref()),
        Cmd::Doctor => {
            if !doctor::print(&doctor::run(&cli.file)) {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Myself {
            action: SelfAction::Uninstall { yes },
        } => selfcmd::uninstall(yes),
        Cmd::Skills {
            action: SkillsAction::Install { dir, force },
        } => {
            let stack = load(&cli.file);
            for o in skills::install(&stack, &cli.file, dir.as_deref(), force)? {
                match o {
                    skills::Outcome::Written(p) => println!("written    {}", p.display()),
                    skills::Outcome::Unchanged(p) => println!("up to date {}", p.display()),
                    skills::Outcome::Kept(p) => println!(
                        "kept       {} (written by the project, not by devstack; --force replaces it)",
                        p.display()
                    ),
                }
            }
            Ok(())
        }
        Cmd::Skills {
            action: SkillsAction::Check,
        } => {
            match skills::outdated_note(&cli.file) {
                Some(n) => println!("{n}"),
                None => println!("skills: up to date (or not installed in the default directory)"),
            }
            Ok(())
        }
        Cmd::ServeStatic { dir, port, no_spa } => {
            devstack_daemon::staticsrv::serve(dir, port, !no_spa).map_err(|e| anyhow::anyhow!(e))
        }
        Cmd::Builds { service, limit } => client::builds(&cli.file, &service, limit),
        Cmd::Top { watch } => topcmd::run(&cli.file, watch),
        Cmd::Run { command, yes } => client::run_command(&cli.file, &command, yes),
        Cmd::Daemon => devstack_daemon::serve(&cli.file).map_err(|e| anyhow::anyhow!(e)),
        Cmd::Build { service, lane } => {
            let stack = load(&cli.file);
            buildcmd::run(&stack, &repo_of(&cli.file)?, &service, &lane)
        }
        Cmd::Env {
            service,
            export,
            out,
        } => {
            let stack = load(&cli.file);
            envcmd::run(
                &stack,
                &repo_of(&cli.file)?,
                &cli.file,
                &service,
                export.is_some(),
                out.as_deref(),
            )
        }
        Cmd::Shim { args } => {
            #[cfg(unix)]
            devstack_proc::shim_main(&args);
            #[cfg(not(unix))]
            {
                let _ = args;
                anyhow::bail!("the shim is only used on Linux")
            }
        }
        Cmd::Schema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&devstack_config::json_schema())?
            );
            Ok(())
        }
    }
}
