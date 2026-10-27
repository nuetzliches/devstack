//! `devstack`: one binary for daemon, CLI and MCP server (ADR-0005, ADR-0007).
//! Phase 1 starts with the commands that need no daemon.

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

mod buildcmd;
mod client;
mod dispatch;
mod doctor;
mod envcmd;
mod hubcmd;
mod init;
mod integration;
mod mcp;
mod poolcmd;
mod selfcmd;
mod tray;
use devstack_skills as skills;
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
    /// Sync the main working tree's snapshot and build a service into an
    /// artifact (ADR-0002). A lane builds in its daemon (`devstack restart
    /// <service>` in its worktree).
    Build { service: String },
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
        /// Run it with this profile of the stack file (ADR-0041): how it runs
        /// for a purpose (an acceptance run). Shown by `status`.
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
        /// With `--profile`: once ready, run the profile's `command` and end
        /// with its result.
        #[arg(long, requires = "profile")]
        run: bool,
        /// Confirm what the profile asks (`data = "fresh"` in the main tree
        /// throws its data away) and what its command asks.
        #[arg(long)]
        yes: bool,
    },
    /// Stop the services, remove the containers (volumes stay) and end the
    /// daemon. In the main working tree, lanes that still run keep the daemon,
    /// what serves them and the `shared` containers they use.
    Down {
        /// Leave the containers running: the next `up` is faster.
        #[arg(long)]
        keep_infra: bool,
        /// What `down` does anyway since 0.25.0 (ADR-0066); kept for scripts.
        #[arg(long, hide = true, conflicts_with = "keep_infra")]
        infra: bool,
        /// The lanes too: the daemon exits even while lanes run. A daemon
        /// that refuses this CLI or hangs is ended by its pid.
        #[arg(long)]
        all: bool,
    },
    /// State of services and infrastructure.
    Status,
    /// Start a service, every service of a group, or one Compose container.
    Start {
        /// A service or a group of the stack file, or a Compose service
        /// (`app.web`, `compose.app.web`, or `web` if one group starts it).
        service: String,
    },
    /// Stop a service, every service of a group, or one Compose container.
    Stop {
        /// A service or a group of the stack file, or a Compose service
        /// (`app.web`, `compose.app.web`, or `web` if one group starts it).
        service: String,
    },
    /// Rebuild and restart a service; for a group every service (built at the same time) and its containers; restart one Compose container.
    Restart {
        /// A service or a group of the stack file, or a Compose service
        /// (`app.web`, `compose.app.web`, or `web` if one group starts it).
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
    /// The tray icon: the stacks' state, the dashboard, start and stop
    /// (ADR-0060). The hub starts it; `devstack hub tray off` keeps it off.
    Tray,
    /// Open the dashboard: starts the hub if needed and pairs the browser
    /// (ADR-0021). In a repository it opens on this stack, in a worktree on
    /// its lane.
    Ui {
        /// Open on this lane of the stack (default: the worktree's own).
        #[arg(long, value_name = "LANE")]
        lane: Option<String>,
        /// Print the lasting links to this stack (or lane) instead: the page,
        /// for a browser paired before, and the `devstack://open` link, which
        /// pairs the browser first.
        #[arg(long, conflicts_with = "print")]
        link: bool,
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
    /// Connect to a lane someone shares with this machine (ADR-0053): each
    /// of its services forwarded here, on its owner's port if free.
    Connect {
        /// The lane's name (or its lease, see `devstack pool shared`).
        lane: String,
    },
    /// Disconnect from a shared lane: its forwards close, its ports here are
    /// free again.
    Disconnect {
        /// The lane's name (or its lease).
        lane: String,
    },
    /// Where this machine's lanes start without `--on` (ADR-0025): `local`
    /// (the default), `pool`, a pool's name, a node or `<pool>:<node>`;
    /// nothing to show it. Setting it is consent to send a worktree there
    /// without being asked.
    DefaultOn { on: Option<String> },
    /// Where a lane could start: this machine, and each node of the pools
    /// with its load, its lanes and the coordinator's order and reason.
    Placement {
        /// The lane (its name, see `devstack lane list`).
        lane: String,
    },
}

#[derive(Subcommand)]
enum LaneAction {
    /// The lanes (the repository's worktrees), their branches and ports.
    List,
    /// Start a lane (its name or branch, see `lane list`) from anywhere in
    /// the repository, as `devstack up` in its worktree does: its own ports
    /// and containers, beside the main tree.
    Start {
        name: String,
        /// Run it on this node of the pool (its name or member id), or
        /// `pool` to let the coordinator choose (ADR-0025).
        #[arg(long, value_name = "NODE")]
        on: Option<String>,
        /// Run it with this profile of the stack file (ADR-0041).
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
        /// With `--profile`: once ready, run the profile's `command` in the
        /// lane and end with its result.
        #[arg(long, requires = "profile")]
        run: bool,
    },
    /// Stop a lane: its services stop and it is no longer started (its
    /// builds stay). The worktree is git's: devstack never creates or removes
    /// one.
    Stop { name: String },
    /// Throw a stopped lane's data away (ADR-0038): its containers and
    /// volumes here and on the nodes it ran on. Its next start is a fresh
    /// one, seeded again; its worktree and builds stay.
    Reset { name: String },
    /// Remove what this repository's lanes leave behind: on this machine
    /// the containers and volumes of lanes whose worktree is gone, on the
    /// pool's nodes this machine's kept lanes of this repository that are
    /// gone, or not used for `--older-than` (ADR-0038); with
    /// `--older-than`, also what `prepare` made in this machine's store and
    /// no copy used for as long (ADR-0062).
    Prune {
        /// Also kept lanes on nodes, and entries of the store, not used for
        /// this long (`7d`, `36h`).
        #[arg(long, value_name = "AGE")]
        older_than: Option<String>,
        /// Only say what would go.
        #[arg(long)]
        dry_run: bool,
    },
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
    /// it in their dashboard, open its services and view its state, logs,
    /// events and usage, read only (ADR-0040). Logs show what the services
    /// print: ask before. Without `--with`: with nobody again.
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
        /// Print a systemd unit that runs this agent with these arguments
        /// and starts it again whenever it ends (an update an admin ordered
        /// included, ADR-0039), instead of running it.
        #[arg(long)]
        print_unit: bool,
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
    /// Grant a member to run the pool's lanes (`on`), or take it back
    /// (`off`): only a granted node is placed on (ADR-0032, addendum).
    Node { member: String, state: String },
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
        /// What of its free CPU and memory the node offers the pool, percent
        /// (default 50).
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
        share_percent: Option<u8>,
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
    /// Desktop notifications from the hub (ADR-0045); without an action, what
    /// notifies and whether the desktop shows it.
    Notify {
        #[command(subcommand)]
        action: Option<NotifyAction>,
    },
    /// The tray icon the hub starts (ADR-0060); without an action, whether
    /// it runs.
    Tray {
        #[command(subcommand)]
        action: Option<TrayAction>,
    },
}

#[derive(Subcommand)]
enum TrayAction {
    /// Start the tray now and with the hub from now on.
    On,
    /// End the tray, and do not start it with the hub.
    Off,
}

#[derive(Subcommand)]
enum NotifyAction {
    /// Switch kinds on: build_failed, held, service_failed, lane_failed,
    /// main_ports, live, or all.
    On {
        #[arg(required = true, value_name = "KIND")]
        kinds: Vec<String>,
    },
    /// Switch kinds off (as `on`).
    Off {
        #[arg(required = true, value_name = "KIND")]
        kinds: Vec<String>,
    },
    /// Show a test notification.
    Test,
}

#[derive(Subcommand)]
enum SelfAction {
    /// Remove devstack for this user: hub, its start at logon and link, the
    /// per-user state, the PATH entry and the installed binary. Repositories keep
    /// their .devstack/ directories. Refuses while a stack or another devstack
    /// process (a node agent, a coordinator, an MCP server) runs.
    Uninstall {
        /// Do not ask.
        #[arg(long)]
        yes: bool,
        /// Remove the pool coordinator kept in the per-user state too (its CA:
        /// every member's certificate becomes worthless). Without it,
        /// uninstalling refuses while there is one.
        #[arg(long)]
        with_coordinator: bool,
    },
}

#[derive(Subcommand)]
enum SkillsAction {
    /// Write the skills `devstack` (run the stack) and `devstack-config` (edit the
    /// stack file), filled in from this stack file. Again to refresh them.
    Install {
        /// Target directory; default the folders `[agents] skills` names (`.claude/skills`).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Also replace a file of the same name the project wrote itself.
        #[arg(long)]
        force: bool,
    },
    /// Do the installed skills match this devstack and this stack file?
    /// Exits 1 when they do not.
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

/// Every command goes to the area that handles it ([`dispatch`]).
fn main() -> Result<()> {
    // Daemons, agents and the hub hold many connections (unix: the soft
    // limit up to the hard one).
    let _ = devstack_proc::raise_open_files();
    let Cli { file, cmd } = Cli::parse();
    match cmd {
        cmd @ (Cmd::Check { .. }
        | Cmd::Build { .. }
        | Cmd::Up { .. }
        | Cmd::Down { .. }
        | Cmd::Status
        | Cmd::Env { .. }
        | Cmd::Schema
        | Cmd::Doctor
        | Cmd::Init { .. }
        | Cmd::Mcp) => dispatch::stack::run(&file, cmd),
        cmd @ (Cmd::Start { .. }
        | Cmd::Stop { .. }
        | Cmd::Restart { .. }
        | Cmd::HotReload { .. }
        | Cmd::Promote { .. }
        | Cmd::Rollback { .. }
        | Cmd::Logs { .. }
        | Cmd::Builds { .. }
        | Cmd::Top { .. }
        | Cmd::Run { .. }) => dispatch::services::run(&file, cmd),
        Cmd::Lane { action } => dispatch::lane::run(&file, action),
        cmd @ (Cmd::Node { .. } | Cmd::Join { .. } | Cmd::Pool { .. } | Cmd::Open { .. }) => {
            dispatch::pool::run(&file, cmd)
        }
        cmd @ (Cmd::Ui { .. } | Cmd::Hub { .. } | Cmd::OpenUrl { .. } | Cmd::Tray) => {
            dispatch::hub::run(&file, cmd)
        }
        cmd @ (Cmd::Update { .. } | Cmd::Myself { .. }) => dispatch::myself::run(cmd),
        Cmd::Skills { action } => dispatch::skills::run(&file, action),
        cmd @ (Cmd::Daemon | Cmd::ServeStatic { .. } | Cmd::Shim { .. }) => {
            dispatch::internal::run(&file, cmd)
        }
    }
}

#[cfg(test)]
mod tests {
    //! The command line's surface, pinned: these parse as they did before,
    //! and what clap refuses stays refused.

    use super::*;
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("devstack").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn refused(args: &[&str]) -> ErrorKind {
        match Cli::try_parse_from(std::iter::once("devstack").chain(args.iter().copied())) {
            Ok(_) => panic!("{args:?} parsed"),
            Err(e) => e.kind(),
        }
    }

    #[test]
    fn the_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn the_stack_file_defaults_and_is_global() {
        assert_eq!(parse(&["status"]).file, PathBuf::from("devstack.toml"));
        let cli = parse(&["restart", "api", "-f", "other.toml", "--rebuild"]);
        assert_eq!(cli.file, PathBuf::from("other.toml"));
        assert!(matches!(
            cli.cmd,
            Cmd::Restart { service, rebuild: true } if service == "api"
        ));
    }

    #[test]
    fn up_takes_its_lane_options() {
        let Cmd::Up {
            workers, force, on, ..
        } = parse(&["up", "--workers", "--on", "pool"]).cmd
        else {
            panic!("not up");
        };
        assert!(workers && !force);
        assert_eq!(on.as_deref(), Some("pool"));
    }

    #[test]
    fn ui_opens_on_a_lane_or_prints_its_links() {
        assert!(matches!(
            parse(&["ui", "--lane", "feat-x", "--link"]).cmd,
            Cmd::Ui { lane: Some(l), link: true, print: false, .. } if l == "feat-x"
        ));
        assert!(matches!(
            parse(&["ui"]).cmd,
            Cmd::Ui {
                lane: None,
                link: false,
                print: false,
                ..
            }
        ));
        assert_eq!(
            refused(&["ui", "--link", "--print"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn promote_takes_a_service_or_a_lane() {
        assert!(matches!(
            parse(&["promote", "api"]).cmd,
            Cmd::Promote { service: Some(s), lane: None } if s == "api"
        ));
        assert!(matches!(
            parse(&["promote", "--lane", "main"]).cmd,
            Cmd::Promote { service: None, lane: Some(l) } if l == "main"
        ));
        assert_eq!(refused(&["promote"]), ErrorKind::MissingRequiredArgument);
        assert_eq!(
            refused(&["promote", "api", "--lane", "x"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn hot_reload_is_on_or_off() {
        assert!(matches!(
            parse(&["hot-reload", "web", "on"]).cmd,
            Cmd::HotReload { service, state } if service == "web" && state == "on"
        ));
        assert_eq!(
            refused(&["hot-reload", "web", "maybe"]),
            ErrorKind::InvalidValue
        );
    }

    #[test]
    fn logs_options_and_their_conflicts() {
        let Cmd::Logs {
            service,
            tail,
            follow,
            grep,
            ..
        } = parse(&["logs", "api", "-n", "50", "--follow", "--grep", "error"]).cmd
        else {
            panic!("not logs");
        };
        assert_eq!(service.as_deref(), Some("api"));
        assert_eq!(tail, 50);
        assert!(follow);
        assert_eq!(grep.as_deref(), Some("error"));
        assert!(matches!(parse(&["logs"]).cmd, Cmd::Logs { tail: 200, .. }));
        assert_eq!(
            refused(&["logs", "--list"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            refused(&["logs", "--daemon", "--hub"]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            refused(&["logs", "api", "--build", "abc", "--follow"]),
            ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn lane_pool_and_node_subcommands() {
        assert!(matches!(
            parse(&["lane", "move", "feature", "--to", "local"]).cmd,
            Cmd::Lane { action: LaneAction::Move { name, to } } if name == "feature" && to == "local"
        ));
        assert!(matches!(
            parse(&["lane", "share", "feature", "--with", "team:qa", "--with", "*"]).cmd,
            Cmd::Lane { action: LaneAction::Share { with, .. } } if with == ["team:qa", "*"]
        ));
        assert!(matches!(
            parse(&["pool", "--pool", "team", "telemetry", "on"]).cmd,
            Cmd::Pool { pool: Some(p), action: Some(PoolAction::Telemetry { state: Some(s) }) }
                if p == "team" && s == "on"
        ));
        assert!(matches!(
            parse(&["pool", "default-on", "pool"]).cmd,
            Cmd::Pool { action: Some(PoolAction::DefaultOn { on: Some(o) }), .. } if o == "pool"
        ));
        assert!(matches!(
            parse(&["pool", "default-on"]).cmd,
            Cmd::Pool {
                action: Some(PoolAction::DefaultOn { on: None }),
                ..
            }
        ));
        assert!(matches!(
            parse(&["pool", "placement", "feature"]).cmd,
            Cmd::Pool { action: Some(PoolAction::Placement { lane }), .. } if lane == "feature"
        ));
        // `--dir` of `node members` is global to its actions.
        assert!(matches!(
            parse(&["node", "members", "approve", "m1", "--admin", "--dir", "coord"]).cmd,
            Cmd::Node { action: NodeAction::Members {
                action: Some(MembersAction::Approve { member, admin: true }),
                dir: Some(d),
            } } if member == "m1" && d == Path::new("coord")
        ));
        assert_eq!(
            refused(&["node", "init"]),
            ErrorKind::MissingRequiredArgument
        );
        let Cmd::Join { pool, no_wait, .. } =
            parse(&["join", "https://coord:7443", "--fingerprint", "ab"]).cmd
        else {
            panic!("not join");
        };
        assert_eq!(pool, "default");
        assert!(!no_wait);
    }

    #[test]
    fn hub_self_skills_and_env() {
        assert!(matches!(parse(&["hub"]).cmd, Cmd::Hub { action: None }));
        assert!(matches!(
            parse(&["hub", "notify", "off", "live", "held"]).cmd,
            Cmd::Hub {
                action: Some(HubAction::Notify {
                    action: Some(NotifyAction::Off { ref kinds })
                })
            } if kinds == &["live", "held"]
        ));
        assert!(matches!(
            parse(&["hub", "install", "--no-autostart"]).cmd,
            Cmd::Hub {
                action: Some(HubAction::Install { no_autostart: true })
            }
        ));
        assert!(matches!(
            parse(&["self", "uninstall", "--yes"]).cmd,
            Cmd::Myself {
                action: SelfAction::Uninstall {
                    yes: true,
                    with_coordinator: false
                }
            }
        ));
        assert!(matches!(
            parse(&["skills", "check"]).cmd,
            Cmd::Skills {
                action: SkillsAction::Check
            }
        ));
        assert!(matches!(
            parse(&["env", "api", "--export", "dotenv", "--out", "api.env"]).cmd,
            Cmd::Env {
                export: Some(ExportFormat::Dotenv),
                out: Some(_),
                ..
            }
        ));
        assert_eq!(refused(&["nosuch"]), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn hidden_commands_still_parse() {
        assert!(matches!(
            parse(&["serve-static", "dist", "--port", "8080", "--no-spa"]).cmd,
            Cmd::ServeStatic {
                port: 8080,
                no_spa: true,
                ..
            }
        ));
        assert!(matches!(
            parse(&["shim", "run", "--flag", "-x"]).cmd,
            Cmd::Shim { args } if args == ["run", "--flag", "-x"]
        ));
        assert!(matches!(parse(&["daemon"]).cmd, Cmd::Daemon));
        assert!(matches!(
            parse(&["open-url", "devstack://hub/start"]).cmd,
            Cmd::OpenUrl { .. }
        ));
    }
}
