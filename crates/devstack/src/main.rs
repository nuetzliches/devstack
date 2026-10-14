//! `devstack`: one binary for daemon, CLI and MCP server (ADR-0005, ADR-0007).
//! Phase 1 starts with the commands that need no daemon.

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

mod buildcmd;
mod client;
mod envcmd;

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
    /// Console output of a service.
    Logs {
        service: String,
        #[arg(long, short = 'n', default_value_t = 200)]
        tail: usize,
        #[arg(long)]
        follow: bool,
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
    /// Print the JSON Schema of the stack file (for editors).
    Schema,
    /// Internal (Linux): the shim between daemon and service (ADR-0009).
    #[command(hide = true)]
    Shim {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
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
        Cmd::Logs {
            service,
            tail,
            follow,
        } => client::logs(&cli.file, &service, tail, follow),
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
