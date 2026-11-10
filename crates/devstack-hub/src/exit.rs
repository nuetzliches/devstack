//! Ending the hub's process (#398): what it said last goes to the pools
//! first. Telemetry is sent once a minute; a `std::process::exit` right
//! after a line (`hub_stop` before an update, `hub_stuck` from the watchdog)
//! dropped it, so the pool never saw why a hub went.

use std::time::Duration;

/// How long an exit waits for the last lines to be sent.
const FLUSH: Duration = Duration::from_secs(2);

/// End the process with `code`, after sending the lines kept for the pools.
/// The only way the hub ends itself; a test keeps it so.
pub(crate) fn exit(code: i32) -> ! {
    devstack_daemon::telemetry::flush(FLUSH);
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    /// Every place the hub ends its own process goes through [`super::exit`]:
    /// a bare `std::process::exit` loses the lines of the last minute.
    #[test]
    fn the_hub_ends_itself_only_through_exit() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut bare = Vec::new();
        for entry in std::fs::read_dir(&src).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") || path.ends_with("exit.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            // Test modules may end a helper process as they like.
            let end = lines
                .windows(2)
                .position(|w| w[0].trim() == "#[cfg(test)]" && w[1].contains("mod "))
                .unwrap_or(lines.len());
            for (i, l) in lines[..end].iter().enumerate() {
                if l.contains("process::exit(") && !l.trim_start().starts_with("//") {
                    bare.push(format!(
                        "{}:{}",
                        path.file_name().unwrap().to_string_lossy(),
                        i + 1
                    ));
                }
            }
        }
        assert!(
            bare.is_empty(),
            "end the hub with exit::exit, not std::process::exit: {bare:?}"
        );
    }
}
