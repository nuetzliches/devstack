//! `devstack self uninstall`: take devstack off this machine for this user.
//!
//! Removes what the installer and devstack itself put into the user's profile:
//! the hub and its tray (stopped), its autostart (on macOS the LaunchAgent) and
//! `devstack://` link, the name its notifications show under (Windows,
//! ADR-0045), the per-user state (token, registry of stacks, hub log and
//! sessions), the PATH entry and the installed binary. A binary winget or
//! Homebrew installed stays for them to remove. It leaves the repositories alone: each keeps its
//! `.devstack/` directory (artifacts, logs) until someone deletes it, and the
//! command lists them so nothing is forgotten.
//!
//! It refuses while a stack still runs (its processes belong to a daemon that
//! the binary being removed started) or another devstack process of this
//! user does (a node agent, a coordinator, an MCP server), and while the
//! per-user state holds a pool coordinator, unless `--with-coordinator`: a
//! coordinator's CA cannot be made again. It names the pool memberships that
//! go with the state.

use anyhow::{Result, anyhow, bail};
use std::io::Write as _;
use std::path::{Path, PathBuf};

fn state_root() -> PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

/// What of the pool lives in the per-user state and goes with it: the
/// coordinator's directory, if it has a CA there, and the pools this machine
/// has a membership of (ADR-0034).
fn pool_state(root: &Path) -> (Option<PathBuf>, Vec<String>) {
    let coordinator = Some(crate::poolcmd::coordinator_dir(None))
        .filter(|d| d.starts_with(root) && d.join("ca.key").exists());
    let memberships = devstack_node::member::pools(root)
        .into_iter()
        .filter(|(_, dir)| dir.starts_with(root) && dir.join("member.json").is_file())
        .map(|(name, _)| name)
        .collect();
    (coordinator, memberships)
}

/// Other devstack processes of this user, besides this one, the hub and
/// the tray (both are stopped): a node agent, a coordinator, an MCP server.
/// They run the binary that is to go, and a running one cannot be deleted
/// (Windows).
fn other_devstack_processes(hub: Option<u32>, tray: Option<u32>) -> Vec<u32> {
    let me = std::process::id();
    devstack_desktop::devstack_pids()
        .into_iter()
        .filter(|p| *p != me && Some(*p) != hub && Some(*p) != tray)
        .collect()
}

pub fn uninstall(yes: bool, with_coordinator: bool) -> Result<()> {
    let stacks = devstack_daemon::registry::list();
    let running: Vec<&devstack_daemon::registry::Entry> = stacks
        .iter()
        .filter(|e| {
            devstack_daemon::read_info(&e.repo.join(".devstack")).is_some_and(|i| pid_alive(i.pid))
        })
        .collect();
    if !running.is_empty() {
        let list: Vec<String> = running
            .iter()
            .map(|e| format!("  {} ({})", e.name, e.repo.display()))
            .collect();
        bail!(
            "stacks still run; stop them first (`devstack down --infra` in each):\n{}",
            list.join("\n")
        );
    }
    let others = other_devstack_processes(
        devstack_hub::read_info().map(|h| h.pid),
        devstack_hub::tray::running(),
    );
    if !others.is_empty() {
        let list: Vec<String> = others.iter().map(u32::to_string).collect();
        bail!(
            "other devstack processes run (a node agent, a coordinator, an MCP server?): pid {}; end them first",
            list.join(", ")
        );
    }
    let root = state_root();
    let (coordinator, memberships) = pool_state(&root);
    if let Some(dir) = &coordinator
        && !with_coordinator
    {
        bail!(
            "{} holds a pool coordinator: its CA cannot be made again, and every member's certificate depends on it. \
             Move it elsewhere (`DEVSTACK_COORDINATOR_DIR`, `devstack node serve --dir`) or uninstall with `--with-coordinator`",
            dir.display()
        );
    }

    let exe = std::env::current_exe()?;
    let ours = devstack_desktop::install_dir().is_some_and(|d| exe.parent() == Some(d.as_path()));
    let winget = devstack_desktop::is_winget_install(&exe);
    let brew = devstack_desktop::homebrew_prefix(&exe).is_some();
    println!("devstack self uninstall removes:");
    if devstack_desktop::LINK_UNAVAILABLE.is_some() {
        println!("  - the dashboard hub, its tray and its start at logon");
    } else {
        println!("  - the dashboard hub, its tray, its start at logon and the devstack:// link");
    }
    println!("  - {} (token, list of stacks, hub log)", root.display());
    if !memberships.is_empty() {
        println!(
            "  - this machine's membership of the pools {} (joining again needs an admin's approval)",
            memberships.join(", ")
        );
    }
    if let Some(dir) = &coordinator {
        println!(
            "  - the pool coordinator in {} with its CA: every member must join a new one",
            dir.display()
        );
    }
    if ours {
        println!("  - {} and its PATH entry", exe.display());
    } else if winget {
        println!(
            "  (not {}: winget installed it, `winget uninstall {}` removes it)",
            exe.display(),
            devstack_desktop::channel::WINGET_ID
        );
    } else if brew {
        println!(
            "  (not {}: Homebrew installed it, `brew uninstall devstack` removes it)",
            exe.display()
        );
    } else {
        println!(
            "  (not {}: it is not where the installer puts devstack)",
            exe.display()
        );
    }
    if !stacks.is_empty() {
        println!("It keeps each repository's .devstack/ directory (artifacts, logs):");
        for e in &stacks {
            println!("  {}", e.repo.join(".devstack").display());
        }
    }
    if !yes && !confirm("Continue? [y/N] ")? {
        println!("nothing removed");
        return Ok(());
    }

    let _ = devstack_hub::tray::stop(std::time::Duration::from_secs(5));
    let _ = crate::hubcmd::stop();
    // The hub holds its log and lock open until it has exited (2 s grace).
    let port = devstack_hub::port();
    let t0 = std::time::Instant::now();
    while devstack_hub::answers(port) && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    crate::integration::uninstall()?;
    devstack_hub::unregister_notifications();
    // The marker of `integration` lives in the state root too; all of it goes.
    let removed = remove_dir(&root);
    if ours {
        devstack_desktop::remove_from_path(exe.parent().unwrap()).map_err(|e| anyhow!(e))?;
        devstack_desktop::remove_binary(&exe).map_err(|e| anyhow!(e))?;
    }
    if removed {
        println!("devstack is uninstalled.");
    } else {
        println!(
            "devstack is uninstalled, except for {} (see above): delete it once nothing holds it",
            root.display()
        );
    }
    if ours && devstack_desktop::BINARY_GOES_LATER {
        println!(
            "{} goes in a few seconds, once this process has exited.",
            exe.parent().unwrap().display()
        );
    }
    if winget {
        println!(
            "The binary stays: `winget uninstall {}` removes it.",
            devstack_desktop::channel::WINGET_ID
        );
    }
    if brew {
        println!("The binary stays: `brew uninstall devstack` removes it.");
    }
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "j" | "ja"))
}

/// Whether `dir` is gone.
fn remove_dir(dir: &Path) -> bool {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("could not remove {}: {e}", dir.display());
            false
        }
        _ => true,
    }
}

use crate::client::pid_alive;
