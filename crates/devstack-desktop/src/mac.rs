//! What the macOS module writes and reads as plain text (the LaunchAgent's
//! property list, AppleScript), here so that it is built and tested on every
//! platform, not only on a Mac.

/// The LaunchAgent that starts the hub at logon: its label, and the file
/// `~/Library/LaunchAgents/<label>.plist`.
pub(crate) const AGENT_LABEL: &str = "it.nuetzliche.devstack.hub";

/// The LaunchAgent's property list: run `args` once at load (the logon, or
/// `launchctl bootstrap`), like the logon entries on Windows and Linux. The
/// hub it starts runs on by itself (`hub start` detaches it), so it is not
/// kept alive, and launchd leaves it running when `hub start` ends
/// (`AbandonProcessGroup`). `path` is the `PATH` it gets: launchd gives an
/// agent `/usr/bin:/bin:/usr/sbin:/sbin` only, without the Docker CLI and the
/// tools a stack runs.
pub(crate) fn launch_agent(label: &str, args: &[&str], path: &str) -> String {
    let args: String = args
        .iter()
        .map(|a| format!("\t\t<string>{}</string>\n", xml(a)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         {args}\
         \t</array>\n\
         \t<key>EnvironmentVariables</key>\n\
         \t<dict>\n\
         \t\t<key>PATH</key>\n\
         \t\t<string>{path}</string>\n\
         \t</dict>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<false/>\n\
         \t<key>AbandonProcessGroup</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        label = xml(label),
        path = xml(path),
    )
}

/// The `PATH` for the LaunchAgent: the one it is installed from (a terminal,
/// which has the user's tools), with Homebrew's and the system's directories
/// where they are missing.
pub(crate) fn agent_path(current: Option<&str>) -> String {
    let mut dirs: Vec<&str> = current
        .unwrap_or("")
        .split(':')
        .filter(|d| !d.is_empty())
        .collect();
    for d in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.join(":")
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `s` as an AppleScript string literal.
pub(crate) fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The folder `choose folder` answered (`POSIX path of`: with a trailing
/// slash and a line end).
pub(crate) fn chosen_folder(stdout: &str) -> Option<String> {
    let p = stdout.trim_end_matches(['\n', '\r']);
    let p = if p.len() > 1 {
        p.strip_suffix('/').unwrap_or(p)
    } else {
        p
    };
    (!p.is_empty()).then(|| p.to_string())
}

/// osascript's error for a dialog the user cancelled (`-128`).
pub(crate) fn cancelled(stderr: &str) -> bool {
    stderr.contains("(-128)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launch_agent_runs_the_hub_once_at_logon() {
        let p = launch_agent(
            AGENT_LABEL,
            &["/opt/homebrew/bin/devstack", "hub", "start"],
            "/opt/homebrew/bin:/usr/bin",
        );
        assert!(p.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(p.contains("\t<key>Label</key>\n\t<string>it.nuetzliche.devstack.hub</string>\n"));
        assert!(p.contains(
            "\t<array>\n\t\t<string>/opt/homebrew/bin/devstack</string>\n\t\t<string>hub</string>\n\t\t<string>start</string>\n\t</array>\n"
        ));
        assert!(p.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(p.contains("<key>KeepAlive</key>\n\t<false/>"));
        assert!(p.contains("<key>AbandonProcessGroup</key>\n\t<true/>"));
        assert!(p.contains("<key>PATH</key>\n\t\t<string>/opt/homebrew/bin:/usr/bin</string>"));
        assert!(p.ends_with("</dict>\n</plist>\n"));
        // A path is text, never markup.
        let odd = launch_agent(AGENT_LABEL, &["/Users/a&b/<x>/devstack"], "/bin");
        assert!(odd.contains("<string>/Users/a&amp;b/&lt;x&gt;/devstack</string>"));
    }

    #[test]
    fn the_agent_finds_homebrew_and_the_system() {
        assert_eq!(
            agent_path(Some("/Users/me/.cargo/bin:/usr/local/bin:/usr/bin")),
            "/Users/me/.cargo/bin:/usr/local/bin:/usr/bin:/opt/homebrew/bin:/bin:/usr/sbin:/sbin"
        );
        assert_eq!(
            agent_path(None),
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        );
    }

    #[test]
    fn applescript_strings_are_quoted() {
        assert_eq!(applescript_string("plain"), "\"plain\"");
        assert_eq!(
            applescript_string(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn the_chosen_folder_loses_its_slash() {
        assert_eq!(
            chosen_folder("/Users/me/src/shop/\n").as_deref(),
            Some("/Users/me/src/shop")
        );
        assert_eq!(chosen_folder("/\n").as_deref(), Some("/"));
        assert_eq!(chosen_folder("\n"), None);
        assert!(cancelled("0:59: execution error: User canceled. (-128)\n"));
        assert!(!cancelled(
            "execution error: No user interaction allowed. (-1713)"
        ));
    }
}
