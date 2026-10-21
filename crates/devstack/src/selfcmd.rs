//! `devstack self uninstall`: take devstack off this machine for this user.
//!
//! Removes what the installer and devstack itself put into the user's profile:
//! the hub (stopped), its autostart and `devstack://` link, the per-user state
//! (token, registry of stacks, hub log and sessions), the PATH entry and the
//! installed binary. It leaves the repositories alone: each keeps its
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

/// Where the installers put devstack; a binary elsewhere (a cargo build) is
/// not deleted.
pub(crate) fn install_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("devstack"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("bin"))
    }
}

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

/// Other devstack processes of this user, besides this one and the hub
/// (which is stopped): a node agent, a coordinator, an MCP server. They run
/// the binary that is to go, and a running one cannot be deleted (Windows).
fn other_devstack_processes(hub: Option<u32>) -> Vec<u32> {
    let me = std::process::id();
    running_devstack_pids()
        .into_iter()
        .filter(|p| *p != me && Some(*p) != hub)
        .collect()
}

#[cfg(windows)]
fn running_devstack_pids() -> Vec<u32> {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => return Vec::new(),
    };
    let out = std::process::Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq devstack.exe",
            "/FI",
            &format!("USERNAME eq {user}"),
            "/FO",
            "CSV",
            "/NH",
        ])
        .output();
    let Ok(out) = out else { return Vec::new() };
    // `"devstack.exe","1234","Console","1","12,345 K"`
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split(',').nth(1)?.trim_matches('"').parse().ok())
        .collect()
}

#[cfg(not(windows))]
fn running_devstack_pids() -> Vec<u32> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let out = std::process::Command::new("ps")
        .args(["-U", &uid.to_string(), "-o", "pid=,comm="])
        .output();
    let Ok(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, comm) = l.trim().split_once(char::is_whitespace)?;
            let name = comm.trim().rsplit('/').next()?;
            (name == "devstack").then(|| pid.parse().ok())?
        })
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
    let others = other_devstack_processes(devstack_hub::read_info().map(|h| h.pid));
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
    let ours = install_dir().is_some_and(|d| exe.parent() == Some(d.as_path()));
    let winget = crate::update::is_winget_install(&exe);
    println!("devstack self uninstall removes:");
    println!("  - the dashboard hub, its start at logon and the devstack:// link");
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
            crate::update::WINGET_ID
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

    let _ = crate::hubcmd::stop();
    // The hub holds its log and lock open until it has exited (2 s grace).
    let port = devstack_hub::port();
    let t0 = std::time::Instant::now();
    while devstack_hub::answers(port) && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    crate::integration::uninstall()?;
    // The marker of `integration` lives in the state root too; all of it goes.
    let removed = remove_dir(&root);
    if ours {
        remove_from_path(exe.parent().unwrap())?;
        remove_binary(&exe)?;
    }
    if removed {
        println!("devstack is uninstalled.");
    } else {
        println!(
            "devstack is uninstalled, except for {} (see above): delete it once nothing holds it",
            root.display()
        );
    }
    if ours && cfg!(windows) {
        println!(
            "{} goes in a few seconds, once this process has exited.",
            exe.parent().unwrap().display()
        );
    }
    if winget {
        println!(
            "The binary stays: `winget uninstall {}` removes it.",
            crate::update::WINGET_ID
        );
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

#[cfg(windows)]
fn remove_from_path(dir: &Path) -> Result<()> {
    // The user's PATH as stored, unexpanded (`%USERPROFILE%\…` entries stay
    // as they are) and written back with its registry type kept
    // (REG_EXPAND_SZ stays REG_EXPAND_SZ); then a broadcast tells running
    // programs (Explorer) that the environment changed. .NET's
    // [Environment]::SetEnvironmentVariable would expand and flatten it.
    let script = format!(
        "$d = '{}'; $k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true); \
         if ($k) {{ $p = $k.GetValue('Path', '', 'DoNotExpandEnvironmentNames'); \
         if ($p) {{ $kind = $k.GetValueKind('Path'); \
         $n = ($p -split ';' | Where-Object {{ $_ -and ($_.TrimEnd('\\') -ne $d.TrimEnd('\\')) }}) -join ';'; \
         if ($n -ne $p) {{ $k.SetValue('Path', $n, $kind); \
         [Environment]::SetEnvironmentVariable('DEVSTACK_PATH_BROADCAST', '1', 'User'); \
         [Environment]::SetEnvironmentVariable('DEVSTACK_PATH_BROADCAST', $null, 'User') }} }}; $k.Close() }}",
        dir.display().to_string().replace('\'', "''")
    );
    let s = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .map_err(|e| anyhow!("powershell: {e}"))?;
    if !s.success() {
        bail!("could not remove {} from the user PATH", dir.display());
    }
    Ok(())
}

#[cfg(not(windows))]
fn remove_from_path(dir: &Path) -> Result<()> {
    // install.sh only notes when ~/.local/bin is not on PATH; it never edits a
    // shell profile, so there is nothing to undo.
    let _ = dir;
    Ok(())
}

#[cfg(windows)]
fn remove_binary(exe: &Path) -> Result<()> {
    // A running .exe cannot be deleted, only renamed. A detached helper deletes
    // the directory once this process has exited.
    let dir = exe.parent().unwrap();
    let mut cmd = remove_dir_later(dir, 3);
    devstack_proc::spawn_detached(&mut cmd)
        .map_err(|e| anyhow!("remove {}: {e}", dir.display()))?;
    Ok(())
}

/// `cmd /c ping … & rmdir /s /q "<dir>"`: waits about `seconds`, then removes
/// `dir`. The command line is passed as it is (`raw_arg`): Rust's quoting of
/// an argument escapes inner quotes as `\"`, which cmd.exe does not know, and
/// `rmdir` then fails on a mangled path.
#[cfg(windows)]
fn remove_dir_later(dir: &Path, seconds: u32) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("cmd");
    cmd.raw_arg(format!(
        "/d /c ping -n {} 127.0.0.1 >nul & rmdir /s /q \"{}\"",
        seconds.max(1),
        dir.display()
    ))
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    cmd
}

#[cfg(not(windows))]
fn remove_binary(exe: &Path) -> Result<()> {
    std::fs::remove_file(exe).map_err(|e| anyhow!("remove {}: {e}", exe.display()))
}

#[cfg(test)]
mod tests {
    /// The helper really removes the directory, also with spaces and
    /// parentheses in its path (the quoting bug left it in place).
    #[cfg(windows)]
    #[test]
    fn the_install_directory_is_removed_later() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("Programs (x86) dev").join("devstack");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("devstack.exe"), b"x").unwrap();
        let status = super::remove_dir_later(&dir, 1).status().unwrap();
        assert!(status.success(), "{status}");
        assert!(!dir.exists(), "{} is still there", dir.display());
    }
}
