//! `devstack update`: install the latest (or a given) release, then move the
//! hub onto the new binary. devstack does it itself, without a shell: it
//! downloads the release archive, checks it against the release's
//! `SHA256SUMS`, unpacks the binary and puts it in place of the installed one
//! (on Windows the running file is renamed out of the way first; it cannot be
//! overwritten). An installer script started from here inherits the caller's
//! shell environment: Windows PowerShell 5.1 started from PowerShell 7 finds
//! the wrong modules and has no `Get-FileHash` (#12).
//!
//! A devstack winget installed is updated by winget (`winget_upgrade`), one
//! Homebrew installed by Homebrew (`brew_upgrade`), so their record of the
//! version stays true.
//!
//! Running stacks keep their daemon (and its version) until they restart; the
//! dashboard offers that ("Restart stack"), the CLI says it.

use anyhow::{Result, anyhow, bail};
use devstack_desktop::channel::WINGET_ID;
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
        Some(v) => {
            let v = v.strip_prefix('v').unwrap_or(v);
            if !release::valid_version(v) {
                bail!("`{v}` is no release version (e.g. 0.9.8)");
            }
            v.to_string()
        }
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
    if devstack_desktop::is_dev_build(&exe) {
        bail!(
            "this is a build from source ({}); update the installed copy, or rebuild",
            exe.display()
        );
    }
    if devstack_desktop::is_cargo_install(&exe) {
        bail!(
            "this devstack came from `cargo install` ({}); update it the same way, or install a release with the installer (it goes elsewhere)",
            exe.display()
        );
    }
    let brew = devstack_desktop::homebrew_prefix(&exe);
    if let (Some(_), Some(v)) = (&brew, version) {
        bail!(
            "Homebrew installed this devstack ({}), and Homebrew installs its formula's latest release only: \
             `devstack update` without `--version {v}` (or `brew upgrade {}`) updates it",
            exe.display(),
            devstack_desktop::channel::BREW_FORMULA
        );
    }
    println!("installing devstack {target} ...");
    let installed = if let Some(prefix) = &brew {
        brew_upgrade(prefix, &target)?
    } else if devstack_desktop::is_winget_install(&exe) {
        winget_upgrade(&exe, version.map(|_| target.as_str()))?
    } else {
        // Where the installers put devstack, or the running copy if it lives
        // elsewhere.
        let into = devstack_desktop::install_dir()
            .map(|d| d.join(EXE))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| exe.clone());
        install(&target, &into)?
    };

    // The hub runs the old binary: stop it, start the new one.
    let _ = crate::hubcmd::stop();
    let port = devstack_hub::port();
    let t0 = std::time::Instant::now();
    while devstack_hub::answers(port) && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // The hub this starts must not inherit our caller's pipe (see
    // `keep_std_handles`): `devstack update | tail` would never end.
    devstack_proc::keep_std_handles();
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

/// An update an admin of the pool ordered this node's agent (ADR-0039): the
/// release (the latest unless named) downloaded, checked against its
/// `SHA256SUMS` and put in place, as `devstack update` does, without the
/// hub. The binary the agent is to run now, or `None` when it runs that
/// release already. An older release only when the order says `downgrade`.
pub fn for_node(
    order: &devstack_node::coordinator::UpdateOrder,
) -> Result<Option<PathBuf>, String> {
    let current = devstack_config::VERSION;
    let target = match &order.version {
        Some(v) => v.strip_prefix('v').unwrap_or(v).to_string(),
        None => release::latest()
            .map_err(|e| format!("cannot ask GitHub for the latest release: {e}"))?,
    };
    if !release::valid_version(&target) {
        return Err(format!("`{target}` is no release version"));
    }
    if target == current {
        return Ok(None);
    }
    if !release::newer(&target, current) && !order.downgrade {
        return Err(format!(
            "{target} is older than the {current} this node runs: an admin orders a downgrade explicitly"
        ));
    }
    let exe = undeleted(std::env::current_exe().map_err(|e| e.to_string())?);
    if devstack_desktop::is_dev_build(&exe) {
        return Err(format!(
            "this agent is a build from source ({})",
            exe.display()
        ));
    }
    if devstack_desktop::is_cargo_install(&exe) {
        return Err(format!(
            "this agent came from `cargo install` ({})",
            exe.display()
        ));
    }
    // The binary the agent runs, wherever it is: its service unit starts that
    // path again (`ExecStart`), so replacing another copy (the installers'
    // `~/.local/bin`) would bring the old version back on the next restart.
    install(&target, &exe)
        .map(Some)
        .map_err(|e| format!("{e:#}"))
}

/// The path a running binary was started from. Linux names a binary that was
/// replaced since it started (`devstack update` in a shell, while the agent
/// runs) `<path> (deleted)`; the new file is at `<path>`.
fn undeleted(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(p) if cfg!(target_os = "linux") => PathBuf::from(p),
        _ => exe,
    }
}

/// Download, check and put release `version` in place of `target`; returns
/// `target`, the path of the new binary.
fn install(version: &str, target: &Path) -> Result<PathBuf> {
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
    println!("  [3/3] installing to {}", target.display());
    replace(target, &binary)?;
    Ok(target.to_path_buf())
}

fn get(url: &str) -> Result<Vec<u8>> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .tls_config(devstack_hub::release::system_tls())
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

/// Update a devstack winget installed, through winget, so winget keeps the
/// version on record. winget cannot replace a binary that runs (the hub,
/// every stack's daemon): `remove_all: Access is denied`, measured with
/// winget 1.29. So the running file moves out of winget's directory first,
/// as `replace` does for the installers' copy (a running executable can be
/// moved on its volume, not overwritten), and back again when winget did
/// not put a new one there. Returns the path of the new binary.
fn winget_upgrade(exe: &Path, version: Option<&str>) -> Result<PathBuf> {
    // The file itself, not winget's link to it.
    let real = plain(std::fs::canonicalize(exe)?);
    let aside_dir = devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .join("replaced");
    std::fs::create_dir_all(&aside_dir)?;
    for e in std::fs::read_dir(&aside_dir)?.flatten() {
        let _ = std::fs::remove_file(e.path()); // still running: next time
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let aside = aside_dir.join(format!("devstack-{stamp}.exe"));
    if let Err(e) = std::fs::rename(&real, &aside) {
        bail!(
            "cannot move the running {} out of winget's way ({e}); stop the hub (`devstack hub stop`) \
             and every stack (`devstack down`), then run `winget upgrade {WINGET_ID}`",
            real.display()
        );
    }
    let mut cmd = std::process::Command::new("winget");
    cmd.args(["upgrade", "--id", WINGET_ID, "--exact", "--silent"])
        .args(["--accept-package-agreements", "--accept-source-agreements"])
        .arg("--disable-interactivity");
    if let Some(v) = version {
        cmd.args(["--version", v]);
    }
    let ran = cmd.status();
    if ran.as_ref().is_ok_and(|s| s.success()) && real.is_file() {
        return Ok(real);
    }
    if !real.exists() {
        let _ = std::fs::rename(&aside, &real);
    }
    match ran {
        Err(e) => bail!("cannot run winget ({e}); devstack is unchanged"),
        Ok(_) => bail!(
            "winget did not upgrade devstack (see above); it is unchanged. A release reaches winget's \
             catalogue a few days after GitHub: `winget upgrade {WINGET_ID}` later picks it up"
        ),
    }
}

/// Update a devstack Homebrew installed under `prefix`, through Homebrew
/// (ADR-0054 point 4), so Homebrew keeps the version on record: `brew
/// upgrade` of the tap's formula. A running binary is no obstacle on macOS
/// and Linux. Returns the path Homebrew keeps (`<prefix>/bin/devstack`),
/// which is what starts the hub again, never the versioned one under
/// `Cellar/`, which `brew cleanup` removes. `target` is the release GitHub
/// names; the tap gets it shortly after.
fn brew_upgrade(prefix: &Path, target: &str) -> Result<PathBuf> {
    use devstack_desktop::channel::{BREW_FORMULA, brew, brew_bin};
    let brew = brew(prefix);
    match std::process::Command::new(&brew)
        .args(["upgrade", BREW_FORMULA])
        .status()
    {
        Err(e) => bail!("cannot run {} ({e}); devstack is unchanged", brew.display()),
        Ok(s) if !s.success() => bail!(
            "Homebrew did not upgrade devstack (see above); it is unchanged. `brew upgrade {BREW_FORMULA}` tries again"
        ),
        Ok(_) => {}
    }
    let installed = brew_bin(prefix);
    // `brew upgrade` succeeds without a newer formula too.
    let now = std::process::Command::new(&installed)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if !now.split_whitespace().any(|w| w == target) {
        bail!(
            "Homebrew has no devstack {target} yet ({} is `{now}`): the tap follows a release on GitHub \
             shortly; `brew upgrade {BREW_FORMULA}` picks it up then",
            installed.display()
        );
    }
    Ok(installed)
}

/// `\\?\C:\...` (what `canonicalize` gives on Windows) as `C:\...`, which
/// every program understands.
fn plain(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}

/// Write `binary` to `path` and make sure it is on disk before it is used.
fn write_synced(path: &Path, binary: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(binary)?;
    f.sync_all()
}

/// Put `binary` at `target`, never leaving a half-written file there: it is
/// written (and synced) next to it first, then renamed into place. Windows: a
/// running executable can be renamed but not overwritten, so the old one
/// moves to `devstack.exe.old-<time>` first (back again if the second rename
/// fails); daemons started from it keep running, and leftovers of earlier
/// updates go once nothing runs them. Elsewhere one rename replaces it,
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
        let new = dir.join(format!("{EXE}.new"));
        if let Err(e) = write_synced(&new, binary) {
            let _ = std::fs::remove_file(&new);
            bail!("write {}: {e}", new.display());
        }
        let moved = target.is_file();
        if moved && let Err(e) = std::fs::rename(target, &old) {
            let _ = std::fs::remove_file(&new);
            bail!("move {} aside: {e}", target.display());
        }
        if let Err(e) = std::fs::rename(&new, target) {
            if moved {
                let _ = std::fs::rename(&old, target);
            }
            let _ = std::fs::remove_file(&new);
            bail!("put {} in place: {e}", target.display());
        }
    } else {
        let tmp = dir.join(format!(".{EXE}.new"));
        write_synced(&tmp, binary)?;
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
        let left: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".new"))
            .collect();
        assert!(left.is_empty(), "no half-written file stays: {left:?}");
    }

    #[test]
    fn a_canonical_windows_path_is_made_plain() {
        assert_eq!(
            plain(PathBuf::from(r"\\?\C:\Users\u\devstack.exe")),
            PathBuf::from(r"C:\Users\u\devstack.exe")
        );
        let unc = PathBuf::from(r"\\?\UNC\host\share\devstack.exe");
        assert_eq!(plain(unc.clone()), unc);
    }

    #[test]
    fn a_replaced_running_binary_is_found_at_its_path() {
        let gone = PathBuf::from("/usr/local/bin/devstack (deleted)");
        let want = if cfg!(target_os = "linux") {
            PathBuf::from("/usr/local/bin/devstack")
        } else {
            gone.clone()
        };
        assert_eq!(undeleted(gone), want);
        let here = PathBuf::from("/usr/local/bin/devstack");
        assert_eq!(undeleted(here.clone()), here);
    }
}
