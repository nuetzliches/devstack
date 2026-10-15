//! `devstack self uninstall`: take devstack off this machine for this user.
//!
//! Removes what the installer and devstack itself put into the user's profile:
//! the hub (stopped), its autostart and `devstack://` link, the per-user state
//! (token, registry of stacks, hub log and sessions), the PATH entry and the
//! installed binary. It leaves the repositories alone: each keeps its
//! `.devstack/` directory (artifacts, logs) until someone deletes it, and the
//! command lists them so nothing is forgotten.
//!
//! It refuses while a stack still runs: its processes belong to a daemon that
//! the binary being removed started.

use anyhow::{Result, anyhow, bail};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Where the installers put devstack; a binary elsewhere (a cargo build) is
/// not deleted.
fn install_dir() -> Option<PathBuf> {
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

pub fn uninstall(yes: bool) -> Result<()> {
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

    let exe = std::env::current_exe()?;
    let ours = install_dir().is_some_and(|d| exe.parent() == Some(d.as_path()));
    println!("devstack self uninstall removes:");
    println!("  - the dashboard hub, its start at logon and the devstack:// link");
    println!(
        "  - {} (token, list of stacks, hub log)",
        state_root().display()
    );
    if ours {
        println!("  - {} and its PATH entry", exe.display());
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
    remove_dir(&state_root());
    if ours {
        remove_from_path(exe.parent().unwrap())?;
        remove_binary(&exe)?;
    }
    println!("devstack is uninstalled.");
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "j" | "ja"))
}

fn remove_dir(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("note: could not remove {}: {e}", dir.display());
    }
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: plain Win32 calls on a handle we open and close here.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(windows))]
    {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}

#[cfg(windows)]
fn remove_from_path(dir: &Path) -> Result<()> {
    // The user's PATH as stored (REG_EXPAND_SZ kept as is): through .NET, which
    // also tells running programs that the environment changed.
    let script = format!(
        "$d = '{}'; $p = [Environment]::GetEnvironmentVariable('Path','User'); \
         if ($p) {{ $n = ($p -split ';' | Where-Object {{ $_ -and ($_.TrimEnd('\\') -ne $d.TrimEnd('\\')) }}) -join ';'; \
         [Environment]::SetEnvironmentVariable('Path', $n, 'User') }}",
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
    let mut cmd = std::process::Command::new("cmd");
    cmd.args([
        "/c",
        &format!(
            "ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"",
            dir.display()
        ),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    devstack_proc::spawn_detached(&mut cmd)
        .map_err(|e| anyhow!("remove {}: {e}", dir.display()))?;
    Ok(())
}

#[cfg(not(windows))]
fn remove_binary(exe: &Path) -> Result<()> {
    std::fs::remove_file(exe).map_err(|e| anyhow!("remove {}: {e}", exe.display()))
}
