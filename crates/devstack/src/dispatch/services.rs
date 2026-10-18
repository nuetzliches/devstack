//! One service (or group) of a running stack: start, stop, restart,
//! hot reload, promote, rollback, its logs and builds, `top`, and the stack
//! file's commands.

use crate::{Cmd, client, topcmd};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Start { service } => client::service_action(file, &service, "start"),
        Cmd::Stop { service } => client::service_action(file, &service, "stop"),
        Cmd::Restart { service, rebuild } => {
            client::service_action(file, &service, if rebuild { "rebuild" } else { "restart" })
        }
        Cmd::HotReload { service, state } => {
            client::service_action(file, &service, &format!("hot-reload-{state}"))
        }
        Cmd::Promote { service, lane } => match lane {
            Some(l) => client::promote_lane(file, &l),
            None => client::service_action(file, &service.unwrap_or_default(), "promote"),
        },
        Cmd::Rollback { service } => client::service_action(file, &service, "rollback"),
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
                client::log_files(file, service.as_deref().unwrap_or_default())
            } else if build.is_some() || since.is_some() || log.is_some() {
                client::logs_kept(
                    file,
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
                    client::state_dir(file)?.join("daemon.log")
                };
                client::diag_log(&path, tail, grep.as_deref(), follow, json)
            } else if let Some(service) = service {
                match grep.as_deref() {
                    Some(g) if follow => {
                        let c = client::need(&client::state_dir(file)?)?;
                        client::follow_logs(&c, &service, tail, Some(g))
                    }
                    Some(g) => client::logs_grep(file, Some(&service), g, tail),
                    None => client::logs(file, &service, tail, follow),
                }
            } else if follow {
                Err(anyhow::anyhow!(
                    "--follow follows one service: name it (or --daemon / --hub)"
                ))
            } else if let Some(g) = grep.as_deref() {
                client::logs_grep(file, None, g, tail)
            } else {
                Err(anyhow::anyhow!(
                    "name a service, search all of them with --grep, or --daemon / --hub for devstack's own log"
                ))
            }
        }
        Cmd::Builds { service, limit } => client::builds(file, &service, limit),
        Cmd::Top { watch } => topcmd::run(file, watch),
        Cmd::Run { command, yes } => client::run_command(file, &command, yes),
        _ => super::misrouted(),
    }
}
