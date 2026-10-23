//! Windows: the registry under `HKCU` for the hub at logon and the
//! `devstack://` link, WinRT toasts, the Explorer folder dialog, the shell's
//! `open` verb, Docker Desktop's own executable.

use crate::{Local, LocalKind, Platform, on_path};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod foreground;
mod notify;

pub use foreground::Front;

pub(crate) const PLATFORM: Platform = Platform::Windows;

pub const PREVIEW: Option<crate::Preview> = None;

pub const LINK_UNAVAILABLE: Option<&str> = None;

pub fn autostart_note() -> Option<String> {
    None
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn no_window(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ------------------------------------------- autostart and the devstack:// link
//
// Both in the user's profile, no admin rights: autostart is the value
// `devstack-hub` of `HKCU\...\CurrentVersion\Run`, the link
// `HKCU\Software\Classes\devstack`. Both run through `conhost.exe
// --headless`: devstack is a console program, and started from the logon or
// from a browser it would otherwise open a terminal window.

const CLASSES: &str = r"HKCU\Software\Classes\devstack";
const RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "devstack-hub";

fn reg(args: &[&str]) -> Result<bool, String> {
    let s = Command::new("reg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("reg.exe: {e}"))?;
    Ok(s.success())
}

fn must(args: &[&str]) -> Result<(), String> {
    if reg(args)? {
        Ok(())
    } else {
        Err(format!("`reg {}` failed", args.join(" ")))
    }
}

/// `"C:\Windows\System32\conhost.exe" --headless "<exe>" <args>`
fn headless(exe: &Path, args: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(
        r#""{root}\System32\conhost.exe" --headless "{}" {args}"#,
        exe.display()
    )
}

pub fn install_link(exe: &Path) -> Result<(), String> {
    must(&["add", CLASSES, "/ve", "/d", "URL:devstack", "/f"])?;
    must(&["add", CLASSES, "/v", "URL Protocol", "/d", "", "/f"])?;
    let cmd = headless(exe, r#"open-url "%1""#);
    must(&[
        "add",
        &format!(r"{CLASSES}\shell\open\command"),
        "/ve",
        "/d",
        &cmd,
        "/f",
    ])
}

pub fn remove_link() -> Result<(), String> {
    if has_link() {
        must(&["delete", CLASSES, "/f"])?;
    }
    Ok(())
}

pub fn has_link() -> bool {
    reg(&["query", &format!(r"{CLASSES}\shell\open\command")]).unwrap_or(false)
}

pub fn install_autostart(exe: &Path) -> Result<(), String> {
    must(&[
        "add",
        RUN,
        "/v",
        RUN_VALUE,
        "/d",
        &headless(exe, "hub start"),
        "/f",
    ])
}

pub fn remove_autostart() -> Result<(), String> {
    if has_autostart() {
        must(&["delete", RUN, "/v", RUN_VALUE, "/f"])?;
    }
    Ok(())
}

pub fn has_autostart() -> bool {
    reg(&["query", RUN, "/v", RUN_VALUE]).unwrap_or(false)
}

// ------------------------------------------------------------ notifications

pub fn notify_sink(port: u16, hooks: crate::notify::Hooks) -> Box<dyn crate::notify::Sink> {
    let registered = notify::register(&hooks);
    Box::new(notify::Toast::new(port, registered, hooks))
}

pub fn notify_unregister() {
    notify::unregister();
}

// ------------------------------------------------------------- folder dialog

/// The Explorer folder picker (`IFileOpenDialog`), with an owner window
/// brought to the front first: the hub has no window of its own and runs in
/// the background, so the dialog would open behind the browser that asked.
pub fn pick_folder() -> Result<Option<String>, String> {
    let owner = Front::take();
    let mut d = rfd::FileDialog::new().set_title("Choose the repository folder (devstack)");
    if let Some(o) = &owner {
        d = d.set_parent(o);
    }
    Ok(d.pick_folder().map(|p| p.to_string_lossy().into_owned()))
}

// ------------------------------------------------------------------ browsers

pub const URL_OPENER: &str = "rundll32";

pub fn open_browser(url: &str) -> std::io::Result<()> {
    Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(drop)
}

pub fn start_browser(program: &str, url: &str) -> std::io::Result<bool> {
    // `start` finds browsers by their registered name (App Paths), which
    // are rarely on PATH; the empty title keeps a quoted path a program.
    // cmd reads the line again: the address in quotes, or an `&` in it
    // (`/auth?code=…&next=…`) would end the command.
    let mut c = Command::new("cmd");
    c.args(["/d", "/c", "start", "", program]);
    c.raw_arg(format!("\"{url}\""));
    c.creation_flags(CREATE_NO_WINDOW);
    Ok(c.status()?.success())
}

/// The installed web app `id` (ADR-0061): the first Chromium-family
/// browser with a profile that has it, and that browser's program.
pub fn dashboard_app(id: &str) -> Option<crate::webapp::WebApp> {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let program_dirs: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .collect();
    crate::webapp::WINDOWS_BROWSERS
        .iter()
        .find_map(|(dir, exe)| {
            let profile = crate::webapp::profiles_with_app(&local.join(dir).join("User Data"), id)
                .into_iter()
                .next()?;
            let program = program_dirs
                .iter()
                .map(|p| p.join(dir).join("Application").join(exe))
                .find(|p| p.is_file())?;
            Some(crate::webapp::WebApp {
                program,
                profile,
                id: id.to_string(),
            })
        })
}

// -------------------------------------------- the user's programs (ADR-0046)

#[derive(Debug, Clone)]
pub enum Launch {
    /// The folder, opened by the shell (Explorer).
    Shell,
    /// Windows Terminal: `wt -d <path>`.
    Wt(PathBuf),
    /// A shell in a console of its own, in the folder (PowerShell).
    Console(PathBuf),
}

pub fn files_and_terminals() -> Vec<Local> {
    let mut out = vec![Local {
        id: "files",
        label: "Explorer",
        kind: LocalKind::Files,
        launch: Launch::Shell,
    }];
    if let Some(wt) = on_path("wt") {
        out.push(Local {
            id: "wt",
            label: "Windows Terminal",
            kind: LocalKind::Terminal,
            launch: Launch::Wt(wt),
        });
    }
    let ps = on_path("pwsh")
        .or_else(|| on_path("powershell"))
        .or_else(|| {
            let root = std::env::var_os("SystemRoot")?;
            let p = PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
            p.is_file().then_some(p)
        });
    if let Some(ps) = ps {
        out.push(Local {
            id: "powershell",
            label: "PowerShell",
            kind: LocalKind::Terminal,
            launch: Launch::Console(ps),
        });
    }
    out
}

pub fn launch(how: &Launch, dir: &Path, path: &str) -> Result<(), String> {
    match how {
        Launch::Shell => shell_open(path),
        Launch::Wt(exe) => Command::new(exe)
            // `;` separates Windows Terminal's commands, also inside a path.
            .arg("-d")
            .arg(path.replace(';', r"\;"))
            .spawn()
            .map(drop)
            .map_err(|e| format!("start Windows Terminal: {e}")),
        Launch::Console(exe) => {
            const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
            Command::new(exe)
                .arg("-NoExit")
                .current_dir(dir)
                .creation_flags(CREATE_NEW_CONSOLE)
                .spawn()
                .map(drop)
                .map_err(|e| format!("start {}: {e}", exe.display()))
        }
    }
}

/// `HKCR\<scheme>` with a `URL Protocol` value: what the shell opens
/// a link of that scheme with, packaged apps included.
pub fn registered_schemes<'a>(schemes: &[&'a str]) -> Vec<&'a str> {
    schemes.iter().copied().filter(|s| registered(s)).collect()
}

fn registered(scheme: &str) -> bool {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CLASSES_ROOT, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    let key_name = wide(scheme);
    let value = wide("URL Protocol");
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: plain registry calls on our own buffers; the key is closed.
    unsafe {
        if RegOpenKeyExW(HKEY_CLASSES_ROOT, key_name.as_ptr(), 0, KEY_READ, &mut key)
            != ERROR_SUCCESS
        {
            return false;
        }
        let found = RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == ERROR_SUCCESS;
        RegCloseKey(key);
        found
    }
}

/// Its launcher in its (per-user) install folder, which starts the
/// version installed last with the arguments it is given.
pub fn github_desktop() -> Option<PathBuf> {
    let p =
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(r"GitHubDesktop\GitHubDesktop.exe");
    p.is_file().then_some(p)
}

pub fn github_desktop_open(exe: &Path, path: &str) -> Result<(), String> {
    Command::new(exe)
        .arg(format!("--cli-open={path}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("start GitHub Desktop: {e}"))
}

pub fn orca() -> Option<PathBuf> {
    on_path("orca").or_else(|| {
        let p = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join(r"Programs\orca\resources\bin\orca.exe");
        p.is_file().then_some(p)
    })
}

/// What double-clicking `target` does: a folder opens in Explorer, a
/// link in the program its scheme names.
pub fn shell_open(target: &str) -> Result<(), String> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let verb = wide("open");
    let file = wide(target);
    // SAFETY: COM for this thread (the shell's handlers may need it),
    // released again; ShellExecuteW on our own buffers.
    let code = unsafe {
        let com = CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let h = ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
        if com >= 0 {
            CoUninitialize();
        }
        h as isize
    };
    // Above 32 is success; below, one of the shell's error codes.
    if code > 32 {
        Ok(())
    } else {
        Err(format!("the shell could not open it (error {code})"))
    }
}

// -------------------------------------------------------------------- Docker

/// `Docker Desktop.exe`: where a machine-wide installation puts it,
/// beside the `docker` on PATH (`<install>\resources\bin\docker.exe`),
/// or where a per-user installation puts it.
fn docker_desktop() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(base) = std::env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(base).join("Docker").join("Docker"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("docker.exe").is_file()
                && let Some(install) = dir.parent().and_then(|r| r.parent())
            {
                candidates.push(install.to_path_buf());
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let programs = PathBuf::from(local).join("Programs");
        candidates.push(programs.join("Docker").join("Docker"));
        candidates.push(programs.join("DockerDesktop"));
    }
    candidates
        .into_iter()
        .map(|d| d.join("Docker Desktop.exe"))
        .find(|p| p.is_file())
}

pub fn docker_starter() -> Option<&'static str> {
    docker_desktop().is_some().then_some("Docker Desktop")
}

pub fn start_docker() -> std::io::Result<()> {
    let exe =
        docker_desktop().ok_or_else(|| std::io::Error::other("Docker Desktop is not installed"))?;
    // Docker Desktop shows its own window and tray icon; it must outlive us.
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    match cmd
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
    {
        Err(e) if e.raw_os_error() == Some(5) => cmd
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map(|_| ()),
        other => other.map(|_| ()),
    }
}

pub fn docker_hint() -> &'static str {
    "install Docker Desktop, or start your Docker engine"
}

pub fn docker_install_hint() -> &'static str {
    "install Docker Desktop"
}

// ------------------------------------------------------------ self uninstall

pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("devstack"))
}

pub fn devstack_pids() -> Vec<u32> {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => return Vec::new(),
    };
    let out = Command::new("tasklist")
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

pub fn remove_from_path(dir: &Path) -> Result<(), String> {
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
    let s = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .map_err(|e| format!("powershell: {e}"))?;
    if !s.success() {
        return Err(format!(
            "could not remove {} from the user PATH",
            dir.display()
        ));
    }
    Ok(())
}

pub const BINARY_GOES_LATER: bool = true;

pub fn remove_binary(exe: &Path) -> Result<(), String> {
    // A running .exe cannot be deleted, only renamed. A detached helper deletes
    // the directory once this process has exited.
    let dir = exe.parent().unwrap();
    let mut cmd = remove_dir_later(dir, 3);
    devstack_proc::spawn_detached(&mut cmd)
        .map_err(|e| format!("remove {}: {e}", dir.display()))?;
    Ok(())
}

/// `cmd /c ping … & rmdir /s /q "<dir>"`: waits about `seconds`, then removes
/// `dir`. The command line is passed as it is (`raw_arg`): Rust's quoting of
/// an argument escapes inner quotes as `\"`, which cmd.exe does not know, and
/// `rmdir` then fails on a mangled path.
fn remove_dir_later(dir: &Path, seconds: u32) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.raw_arg(format!(
        "/d /c ping -n {} 127.0.0.1 >nul & rmdir /s /q \"{}\"",
        seconds.max(1),
        dir.display()
    ))
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    cmd
}

#[cfg(test)]
mod tests {
    /// The helper really removes the directory, also with spaces and
    /// parentheses in its path (the quoting bug left it in place).
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
