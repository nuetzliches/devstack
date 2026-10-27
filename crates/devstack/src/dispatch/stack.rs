//! The stack as a whole: the stack file, up and down, its status, its
//! environment, the MCP server, and what needs no daemon (init, doctor).

use crate::{Cmd, client, doctor, envcmd, init, load, mcp, repo_of};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Check { compose } => check(file, compose),
        Cmd::Up {
            workers,
            force,
            on,
            profile,
            run,
            yes,
        } => client::up(
            file,
            workers,
            force,
            on.as_deref(),
            profile.as_deref(),
            run,
            yes,
        ),
        Cmd::Down {
            keep_infra, all, ..
        } => client::down(file, !keep_infra, all),
        Cmd::Journal { lines } => {
            let c = client::need(&client::state_dir(file)?)?;
            print!("{}", client::journal_text(&c.journal()?, lines));
            Ok(())
        }
        Cmd::Status => {
            let c = client::need(&client::state_dir(file)?)?;
            client::print_status_for(&c.stack()?, file);
            Ok(())
        }
        Cmd::Mcp => mcp::serve(file),
        Cmd::Init { dir, print, json } => {
            let repo = match dir {
                Some(d) => d,
                None => std::env::current_dir()?,
            };
            init::run(&repo, print, json)
        }
        Cmd::Doctor => {
            if !doctor::print(&doctor::run(file)) {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Build { service } => {
            let stack = load(file);
            crate::buildcmd::run(&stack, &repo_of(file)?, &service)
        }
        Cmd::Env {
            service,
            export,
            out,
        } => {
            let stack = load(file);
            envcmd::run(
                &stack,
                &repo_of(file)?,
                file,
                &service,
                export.is_some(),
                out.as_deref(),
            )
        }
        Cmd::Schema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&devstack_config::json_schema())?
            );
            Ok(())
        }
        _ => super::misrouted(),
    }
}

/// `devstack check`: the stack file, and with `--compose` every Compose group.
fn check(file: &Path, compose: bool) -> Result<()> {
    let stack = load(file);
    println!(
        "{}: ok ({} compose group(s), {} service(s), {} command(s))",
        file.display(),
        stack.compose.len(),
        stack.services.len(),
        stack.commands.len()
    );
    if let Ok(text) = std::fs::read_to_string(file) {
        for w in devstack_config::compat::warnings(&text) {
            println!("warning: {w}");
        }
    }
    for w in devstack_config::validate::warnings(&stack) {
        println!("warning: {w}");
    }
    for w in devstack_config::validate::repo_warnings(&stack, &repo_of(file)?) {
        println!("warning: {w}");
    }
    if compose {
        let repo = repo_of(file)?;
        let docker = devstack_compose::Docker::default();
        let mut failed = false;
        for (name, g) in &stack.compose {
            let group = devstack_compose::Group::from_config(name, g, &repo);
            // Announced ports (ADR-0057) are published 1:1 on fixed ports:
            // what a start checks, on the model it reads.
            let announced = g.announced();
            let checked = docker.validate(&group).and_then(|()| {
                if announced.is_empty() {
                    return Ok(Ok(()));
                }
                docker
                    .config(&group)
                    .map(|m| devstack_compose::check_announced(&m, &announced))
            });
            match checked {
                Ok(Err(e)) => {
                    failed = true;
                    eprintln!("compose.{name}: {e}");
                }
                Ok(Ok(())) => println!("compose.{name}: ok"),
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
            let group =
                devstack_compose::Group::from_config(&r.group, &stack.compose[&r.group], &repo);
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
