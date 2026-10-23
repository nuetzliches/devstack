//! The dashboard's installed web app (ADR-0061): its id, and what each
//! platform's module needs to find and start it. Built and tested on every
//! platform; `linux.rs` and `windows.rs` use their parts.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// An installed web app of a Chromium-family browser, and how to start it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebApp {
    /// The browser's program.
    pub program: PathBuf,
    /// The browser profile it is installed in (`Default`, `Profile 1`).
    pub profile: String,
    /// The app's id (32 letters `a`–`p`).
    pub id: String,
}

impl WebApp {
    /// The browser's arguments that open the app on `url` (in its scope).
    pub fn args(&self, url: &str) -> Vec<String> {
        vec![
            format!("--profile-directory={}", self.profile),
            format!("--app-id={}", self.id),
            format!("--app-launch-url-for-shortcuts-menu-item={url}"),
        ]
    }

    /// Start the app on `url`, in the background. A browser already running
    /// takes the command line over and the started process ends.
    pub fn open(&self, url: &str) -> std::io::Result<()> {
        let mut cmd = Command::new(&self.program);
        cmd.args(self.args(url));
        let mut child = cmd.spawn()?;
        // Reaped, so a long-lived caller (the tray) keeps no zombie.
        std::thread::spawn(move || child.wait());
        Ok(())
    }
}

/// The manifest id of the dashboard the hub serves on `origin`
/// (`http://127.0.0.1:4250`): the manifest's `id`, `/`, resolved.
pub fn manifest_id(origin: &str) -> String {
    format!("{}/", origin.trim_end_matches('/'))
}

/// The origin of a page's `url` (`http://127.0.0.1:4250/auth?…` →
/// `http://127.0.0.1:4250`).
pub fn origin_of(url: &str) -> Option<&str> {
    let rest = url.find("://")? + 3;
    let end = url[rest..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| rest + i);
    (end > rest).then(|| &url[..end])
}

/// The id Chromium gives the web app of `manifest_id`: SHA-256 of the
/// SHA-256 of it, the first 16 bytes as hex digits written `a`–`p`.
pub fn app_id(manifest_id: &str) -> String {
    let inner = Sha256::digest(manifest_id.as_bytes());
    let outer = Sha256::digest(inner);
    outer[..16]
        .iter()
        .flat_map(|b| [b >> 4, b & 0xf])
        .map(|n| char::from(b'a' + n))
        .collect()
}

// --------------------------------------------------- Linux: .desktop files

/// A launcher's `Exec` line in words, as the Desktop Entry spec quotes them:
/// double quotes group, a backslash in them escapes the next character.
/// Field codes (`%U`) are dropped.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn exec_words(exec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(word);
    }
    out.retain(|w| !(w.len() == 2 && w.starts_with('%')));
    out
}

/// The program, profile and app id of a launcher's `[Desktop Entry]`, if
/// its `Exec` starts a web app (`--app-id=`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn desktop_entry_app(text: &str) -> Option<WebApp> {
    let mut in_entry = false;
    let mut exec = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some(v) = line.strip_prefix("Exec=") {
            exec = Some(v);
        }
    }
    let words = exec_words(exec?);
    let (program, args) = words.split_first()?;
    let flag = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(name))
            .map(str::to_string)
    };
    Some(WebApp {
        program: PathBuf::from(program),
        id: flag("--app-id=")?,
        profile: flag("--profile-directory=").unwrap_or_else(|| "Default".into()),
    })
}

/// The desktop folder from `user-dirs.dirs` (`XDG_DESKTOP_DIR="$HOME/…"`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn desktop_dir(user_dirs: &str, home: &Path) -> Option<PathBuf> {
    let v = user_dirs
        .lines()
        .find_map(|l| l.trim().strip_prefix("XDG_DESKTOP_DIR="))?
        .trim()
        .trim_matches('"');
    if let Some(rest) = v.strip_prefix("$HOME") {
        Some(home.join(rest.trim_start_matches('/')))
    } else {
        v.starts_with('/').then(|| PathBuf::from(v))
    }
}

/// The browser programs a launcher's file name (`brave-<id>-Default.desktop`)
/// stands for, to look up on `PATH` when its program is gone.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn programs_for_launcher(file_name: &str) -> &'static [&'static str] {
    const FAMILIES: &[(&str, &[&str])] = &[
        ("brave-", &["brave-browser", "brave"]),
        ("chrome-", &["google-chrome", "google-chrome-stable"]),
        ("chromium-", &["chromium", "chromium-browser"]),
        ("msedge-", &["microsoft-edge", "microsoft-edge-stable"]),
    ];
    FAMILIES
        .iter()
        .find(|(p, _)| file_name.starts_with(p))
        .map_or(&[], |(_, names)| names)
}

/// `/snap/<name>/<revision>/…` → `/snap/bin/<name>`: a launcher names the
/// snap's revision at install, which its updates remove.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn snap_command(program: &Path) -> Option<PathBuf> {
    let rest = program.to_str()?.strip_prefix("/snap/")?;
    let name = rest.split('/').next()?;
    (!name.is_empty() && name != "bin").then(|| PathBuf::from(format!("/snap/bin/{name}")))
}

// ------------------------------------------- Windows: the profile folders

/// The Chromium-family browsers on Windows, in the order they are tried:
/// the `User Data` folder under `%LOCALAPPDATA%`, and the program under
/// `%ProgramFiles%`, `%ProgramFiles(x86)%` or `%LOCALAPPDATA%`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const WINDOWS_BROWSERS: &[(&str, &str)] = &[
    (r"Google\Chrome", "chrome.exe"),
    (r"Microsoft\Edge", "msedge.exe"),
    (r"BraveSoftware\Brave-Browser", "brave.exe"),
    ("Chromium", "chrome.exe"),
];

/// The profiles in a `User Data` folder that have the app `id` installed,
/// `Default` first: `<profile>\Web Applications\Manifest Resources\<id>`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn profiles_with_app(user_data: &Path, id: &str) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(user_data) else {
        return Vec::new();
    };
    let mut found: Vec<String> = dir
        .flatten()
        .filter(|e| {
            e.path()
                .join("Web Applications")
                .join("Manifest Resources")
                .join(id)
                .is_dir()
        })
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    found.sort_by_key(|p| (p != "Default", p.clone()));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_id_is_the_one_chromium_gives_the_hubs_dashboard() {
        // Read from a Brave that installed the dashboard from the hub.
        assert_eq!(
            app_id(&manifest_id("http://127.0.0.1:4250")),
            "imbjdaboabifmbceekoampobpbpodgfh"
        );
        assert_ne!(
            app_id(&manifest_id("http://localhost:4250")),
            app_id(&manifest_id("http://127.0.0.1:4250"))
        );
        assert_eq!(
            manifest_id("http://127.0.0.1:4250/"),
            "http://127.0.0.1:4250/"
        );
    }

    #[test]
    fn the_origin_is_read_from_the_pairing_link() {
        assert_eq!(
            origin_of("http://127.0.0.1:4250/auth?code=x&next=%2F%23%2F"),
            Some("http://127.0.0.1:4250")
        );
        assert_eq!(
            origin_of("http://127.0.0.1:4251"),
            Some("http://127.0.0.1:4251")
        );
        assert_eq!(
            origin_of("http://127.0.0.1:4250?x"),
            Some("http://127.0.0.1:4250")
        );
        assert_eq!(origin_of("no-scheme"), None);
        assert_eq!(origin_of("http:///x"), None);
    }

    #[test]
    fn a_launcher_brave_wrote_is_read() {
        let text = "#!/usr/bin/env xdg-open\n[Desktop Entry]\nVersion=1.0\nTerminal=false\n\
            Type=Application\nName=devstack\n\
            Exec=/snap/brave/698/opt/brave.com/brave/brave-browser --profile-directory=Default --app-id=imbjdaboabifmbceekoampobpbpodgfh\n\
            Icon=brave-imbjdaboabifmbceekoampobpbpodgfh-Default\n\
            StartupWMClass=crx_imbjdaboabifmbceekoampobpbpodgfh\n";
        let app = desktop_entry_app(text).unwrap();
        assert_eq!(
            app.program,
            PathBuf::from("/snap/brave/698/opt/brave.com/brave/brave-browser")
        );
        assert_eq!(app.profile, "Default");
        assert_eq!(app.id, "imbjdaboabifmbceekoampobpbpodgfh");
        assert_eq!(
            app.args("http://127.0.0.1:4250/auth?code=c"),
            [
                "--profile-directory=Default",
                "--app-id=imbjdaboabifmbceekoampobpbpodgfh",
                "--app-launch-url-for-shortcuts-menu-item=http://127.0.0.1:4250/auth?code=c",
            ]
        );
    }

    #[test]
    fn a_launcher_with_quotes_field_codes_and_other_sections() {
        let text = "[Desktop Entry]\n\
            Exec=\"/opt/google/chrome/google-chrome\" \"--profile-directory=Profile 2\" --app-id=abc %U\n\
            [Desktop Action new]\nExec=/usr/bin/other --app-id=zzz\n";
        let app = desktop_entry_app(text).unwrap();
        assert_eq!(
            app.program,
            PathBuf::from("/opt/google/chrome/google-chrome")
        );
        assert_eq!(app.profile, "Profile 2");
        assert_eq!(app.id, "abc");
        assert_eq!(exec_words(r#"a "b \"c\" d" e"#), ["a", r#"b "c" d"#, "e"]);
        // Not a web app: a browser's own launcher.
        assert_eq!(
            desktop_entry_app("[Desktop Entry]\nExec=/usr/bin/brave-browser %U\n"),
            None
        );
        assert_eq!(desktop_entry_app("[Desktop Entry]\nName=x\n"), None);
    }

    #[test]
    fn the_desktop_folder_comes_from_user_dirs() {
        let home = Path::new("/home/u");
        let dirs = "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\n\
            XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n";
        assert_eq!(
            desktop_dir(dirs, home),
            Some(PathBuf::from("/home/u/Schreibtisch"))
        );
        assert_eq!(
            desktop_dir("XDG_DESKTOP_DIR=\"/data/desk\"", home),
            Some(PathBuf::from("/data/desk"))
        );
        assert_eq!(desktop_dir("XDG_MUSIC_DIR=\"$HOME/M\"", home), None);
    }

    #[test]
    fn a_stale_program_has_a_stand_in() {
        assert_eq!(
            snap_command(Path::new(
                "/snap/brave/603/opt/brave.com/brave/brave-browser"
            )),
            Some(PathBuf::from("/snap/bin/brave"))
        );
        assert_eq!(snap_command(Path::new("/snap/bin/brave")), None);
        assert_eq!(snap_command(Path::new("/opt/google/chrome/chrome")), None);
        assert_eq!(
            programs_for_launcher("brave-imbj-Default.desktop"),
            ["brave-browser", "brave"]
        );
        assert_eq!(
            programs_for_launcher("chrome-imbj-Profile_1.desktop")[0],
            "google-chrome"
        );
        assert!(programs_for_launcher("org.gnome.Nautilus.desktop").is_empty());
    }

    #[test]
    fn the_profiles_with_the_app_are_found_default_first() {
        let dir = tempfile::tempdir().unwrap();
        let app = |profile: &str, id: &str| {
            std::fs::create_dir_all(
                dir.path()
                    .join(profile)
                    .join("Web Applications")
                    .join("Manifest Resources")
                    .join(id),
            )
            .unwrap();
        };
        app("Profile 1", "abc");
        app("Default", "abc");
        app("Profile 2", "other");
        assert_eq!(
            profiles_with_app(dir.path(), "abc"),
            ["Default", "Profile 1"]
        );
        assert!(profiles_with_app(dir.path(), "none").is_empty());
        assert!(profiles_with_app(&dir.path().join("missing"), "abc").is_empty());
    }
}
