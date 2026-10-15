//! `devstack update`: install the latest (or a given) release with the official
//! installer, then move the hub onto the new binary. The installer is the one
//! the landing page shows, so an update checks the same checksums and keeps
//! the hub's registrations, exactly like a first install.
//!
//! Running stacks keep their daemon (and its version) until they restart; the
//! dashboard offers that ("Restart stack"), the CLI says it.

use anyhow::{Result, anyhow, bail};
use devstack_hub::release;

const BASE: &str = "https://github.com/nuetzliches/devstack/releases";

pub fn run(check_only: bool, version: Option<&str>) -> Result<()> {
    let current = devstack_config::VERSION;
    let target = match version {
        Some(v) => v.trim_start_matches('v').to_string(),
        None => release::latest()
            .map_err(|e| anyhow!("cannot ask GitHub for the latest release: {e}"))?,
    };
    if check_only {
        if release::newer(&target, current) {
            println!("devstack {target} is out (this is {current}); `devstack update` installs it");
        } else {
            println!("devstack {current} is up to date (latest release: {target})");
        }
        return Ok(());
    }
    if version.is_none() && !release::newer(&target, current) {
        println!("devstack {current} is up to date");
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    if crate::integration::is_dev_build(&exe) {
        bail!(
            "this is a build from source ({}); update the installed copy, or rebuild",
            exe.display()
        );
    }
    println!("installing devstack {target} ...");
    install(version.map(|_| target.as_str()))?;

    // The hub runs the old binary: stop it, start the new one.
    let _ = crate::hubcmd::stop();
    let port = devstack_hub::port();
    let t0 = std::time::Instant::now();
    while devstack_hub::answers(port) && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // The installer may have renamed the running binary out of the way; the new
    // one is at the installer's place.
    let installed = crate::selfcmd::install_dir()
        .map(|d| {
            d.join(if cfg!(windows) {
                "devstack.exe"
            } else {
                "devstack"
            })
        })
        .filter(|p| p.is_file())
        .unwrap_or(exe);
    let started = std::process::Command::new(&installed)
        .args(["hub", "start"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !started {
        eprintln!("note: start the hub again with `devstack hub start`");
    }
    println!(
        "devstack {target} is installed. Running stacks keep their version until they restart: \
         `devstack down` + `devstack up`, or \"Restart stack\" in the dashboard."
    );
    Ok(())
}

#[cfg(windows)]
fn install(version: Option<&str>) -> Result<()> {
    let script = match version {
        Some(v) => format!(
            "& ([scriptblock]::Create((Invoke-RestMethod '{BASE}/download/v{v}/install.ps1'))) -Version {v}"
        ),
        None => {
            format!("Invoke-RestMethod '{BASE}/latest/download/install.ps1' | Invoke-Expression")
        }
    };
    let s = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .status()
        .map_err(|e| anyhow!("powershell: {e}"))?;
    if !s.success() {
        bail!("the installer failed ({s})");
    }
    Ok(())
}

#[cfg(not(windows))]
fn install(version: Option<&str>) -> Result<()> {
    let (url, args) = match version {
        Some(v) => (
            format!("{BASE}/download/v{v}/install.sh"),
            format!(" -s -- --version {v}"),
        ),
        None => (format!("{BASE}/latest/download/install.sh"), String::new()),
    };
    let s = std::process::Command::new("sh")
        .args(["-c", &format!("curl -fsSL '{url}' | sh{args}")])
        .status()
        .map_err(|e| anyhow!("sh: {e}"))?;
    if !s.success() {
        bail!("the installer failed ({s})");
    }
    Ok(())
}
