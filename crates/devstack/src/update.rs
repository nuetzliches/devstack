//! `devstack update`: install the latest (or a given) release, then move the
//! hub onto the new binary. devstack does it itself, without a shell: it
//! downloads the release archive, checks it against the release's
//! `SHA256SUMS`, unpacks the binary and puts it in place of the installed one
//! (on Windows the running file is renamed out of the way first; it cannot be
//! overwritten). An installer script started from here inherits the caller's
//! shell environment: Windows PowerShell 5.1 started from PowerShell 7 finds
//! the wrong modules and has no `Get-FileHash` (#12).
//!
//! Running stacks keep their daemon (and its version) until they restart; the
//! dashboard offers that ("Restart stack"), the CLI says it.

use anyhow::{Result, anyhow, bail};
use devstack_hub::release;
use std::path::{Path, PathBuf};

const BASE: &str = "https://github.com/nuetzliches/devstack/releases";

const ASSET: &str = if cfg!(windows) {
    "devstack-x86_64-pc-windows-msvc.zip"
} else {
    "devstack-x86_64-unknown-linux-musl.tar.gz"
};
const EXE: &str = if cfg!(windows) {
    "devstack.exe"
} else {
    "devstack"
};

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
    let installed = install(&target, &exe)?;

    // The hub runs the old binary: stop it, start the new one.
    let _ = crate::hubcmd::stop();
    let port = devstack_hub::port();
    let t0 = std::time::Instant::now();
    while devstack_hub::answers(port) && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
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

/// Download, check and put release `version` in place; returns the path of
/// the new binary: where the installers put devstack, or `running` if it
/// lives elsewhere.
fn install(version: &str, running: &Path) -> Result<PathBuf> {
    if !cfg!(any(windows, target_os = "linux")) || !cfg!(target_arch = "x86_64") {
        bail!("there is no release build for this platform; build devstack from source");
    }
    let base = format!("{BASE}/download/v{version}");
    println!("  [1/3] downloading {ASSET}");
    let archive = get(&format!("{base}/{ASSET}"))?;
    let sums = String::from_utf8(get(&format!("{base}/SHA256SUMS"))?)?;
    println!("  [2/3] checking it against the release checksums");
    let expected = expected_sum(&sums, ASSET)
        .ok_or_else(|| anyhow!("SHA256SUMS of v{version} lists no {ASSET}"))?;
    let actual = sha256_hex(&archive);
    if actual != expected {
        bail!("checksum mismatch for {ASSET}: expected {expected}, got {actual}");
    }
    let binary = unpack(&archive)?;
    let target = crate::selfcmd::install_dir()
        .map(|d| d.join(EXE))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| running.to_path_buf());
    println!("  [3/3] installing to {}", target.display());
    replace(&target, &binary)?;
    Ok(target)
}

fn get(url: &str) -> Result<Vec<u8>> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .build()
        .new_agent();
    let mut r = agent
        .get(url)
        .header(
            "User-Agent",
            &format!("devstack/{}", devstack_config::VERSION),
        )
        .call()
        .map_err(|e| anyhow!("download {url}: {e}"))?;
    r.body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| anyhow!("download {url}: {e}"))
}

/// The hash `SHA256SUMS` gives for `asset` (`<hex>  <name>` or `<hex> *<name>`).
fn expected_sum(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, name) = l.trim().split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == asset).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(windows)]
fn unpack(archive: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        if f.name()?.rsplit(['/', '\\']).next() == Some(EXE) {
            let mut out = Vec::new();
            f.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    bail!("the archive holds no {EXE}")
}

#[cfg(not(windows))]
fn unpack(archive: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut e = entry?;
        if e.path()?.file_name().and_then(|n| n.to_str()) == Some(EXE) {
            let mut out = Vec::new();
            e.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    bail!("the archive holds no {EXE}")
}

/// Put `binary` at `target`. Windows: a running executable can be renamed but
/// not overwritten, so the old one moves to `devstack.exe.old-<time>`; daemons
/// started from it keep running, and leftovers of earlier updates go once
/// nothing runs them. Elsewhere: written next to it and renamed over it,
/// which running processes do not notice.
fn replace(target: &Path, binary: &[u8]) -> Result<()> {
    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("{} has no directory", target.display()))?;
    std::fs::create_dir_all(dir)?;
    if cfg!(windows) {
        for e in std::fs::read_dir(dir)?.flatten() {
            if e.file_name()
                .to_string_lossy()
                .starts_with("devstack.exe.old")
            {
                let _ = std::fs::remove_file(e.path()); // still running: next time
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let old = dir.join(format!("devstack.exe.old-{stamp}"));
        let moved = target.is_file();
        if moved {
            std::fs::rename(target, &old)?;
        }
        if let Err(e) = std::fs::write(target, binary) {
            if moved {
                let _ = std::fs::rename(&old, target);
            }
            bail!("write {}: {e}", target.display());
        }
    } else {
        let tmp = dir.join(format!(".{EXE}.new"));
        std::fs::write(&tmp, binary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, target)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_are_found_in_both_formats() {
        let sums = "AB12  devstack-x86_64-unknown-linux-musl.tar.gz\ncd34 *devstack-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            expected_sum(sums, "devstack-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("cd34")
        );
        assert_eq!(
            expected_sum(sums, "devstack-x86_64-unknown-linux-musl.tar.gz").as_deref(),
            Some("ab12")
        );
        assert_eq!(expected_sum(sums, "other"), None);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_binary_is_replaced_in_place_twice() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join(EXE);
        std::fs::write(&t, b"old").unwrap();
        replace(&t, b"new").unwrap();
        assert_eq!(std::fs::read(&t).unwrap(), b"new");
        replace(&t, b"newer").unwrap();
        assert_eq!(std::fs::read(&t).unwrap(), b"newer");
    }
}
