//! `devstack`: one binary for daemon, CLI and MCP server (ADR-0005, ADR-0007).
//! Phase 1 starts with the commands that need no daemon.

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

mod buildcmd;
mod client;
mod envcmd;
mod hubcmd;
mod mcp;
mod topcmd;

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
    Up,
    /// Stop the services and the daemon; `--infra` also removes the containers.
    Down {
        #[arg(long)]
        infra: bool,
    },
    /// State of services and infrastructure.
    Status,
    Start {
        service: String,
    },
    Stop {
        service: String,
    },
    Restart {
        service: String,
    },
    /// Take over a candidate that waits for promotion (a schema change, ADR-0013).
    Promote {
        service: String,
    },
    /// Go back to the build that ran before the last replacement (no build, no jobs).
    Rollback {
        service: String,
    },
    /// Console output of a service.
    Logs {
        service: String,
        #[arg(long, short = 'n', default_value_t = 200)]
        tail: usize,
        #[arg(long)]
        follow: bool,
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
    },
    /// The hub that serves the dashboard for every stack; without an action it
    /// runs in the foreground (normally started by `devstack up` / `devstack ui`).
    Hub {
        #[command(subcommand)]
        action: Option<HubAction>,
    },
    /// Print the JSON Schema of the stack file (for editors).
    Schema,
    /// Internal (Linux): the shim between daemon and service (ADR-0009).
    #[command(hide = true)]
    Shim {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum HubAction {
    /// Is the hub running, and where.
    Status,
    /// Stop the hub (stacks keep running).
    Stop,
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
        Cmd::Up => client::up(&cli.file),
        Cmd::Down { infra } => client::down(&cli.file, infra),
        Cmd::Status => {
            let c = client::need(&client::state_dir(&cli.file)?)?;
            client::print_status(&c.stack()?);
            Ok(())
        }
        Cmd::Start { service } => client::service_action(&cli.file, &service, "start"),
        Cmd::Stop { service } => client::service_action(&cli.file, &service, "stop"),
        Cmd::Restart { service } => client::service_action(&cli.file, &service, "restart"),
        Cmd::Promote { service } => client::service_action(&cli.file, &service, "promote"),
        Cmd::Rollback { service } => client::service_action(&cli.file, &service, "rollback"),
        Cmd::Logs {
            service,
            tail,
            follow,
        } => client::logs(&cli.file, &service, tail, follow),
        Cmd::Mcp => mcp::serve(&cli.file),
        Cmd::Ui { print } => hubcmd::ui(print),
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
