//! `devstack doctor`: is this machine ready for this stack? One line per check,
//! with what to do when it is not. Read-only: it starts and changes nothing.
//! The same report is the MCP tool `doctor`, so an agent can check before it
//! starts guessing.

use devstack_compose::engine::{self, Engine};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Compose features stack files rely on: `!reset`, `!override` and
/// `required: false` in overlays (plan O-9).
const MIN_COMPOSE: (u64, u64, u64) = (2, 24, 4);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

pub struct Finding {
    pub level: Level,
    pub text: String,
}

fn f(level: Level, text: impl Into<String>) -> Finding {
    Finding {
        level,
        text: text.into(),
    }
}

pub fn run(stack_file: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let version = devstack_config::VERSION;
    let stack = if stack_file.exists() {
        match devstack_config::load(stack_file) {
            Ok(s) => {
                for w in std::fs::read_to_string(stack_file)
                    .map(|t| devstack_config::compat::warnings(&t))
                    .unwrap_or_default()
                {
                    out.push(f(Level::Warn, w.to_string()));
                }
                out.push(f(
                    Level::Ok,
                    format!(
                        "devstack {version}; {} is valid ({} compose group(s), {} service(s), {} command(s))",
                        stack_file.display(),
                        s.compose.len(),
                        s.services.len(),
                        s.commands.len()
                    ),
                ));
                Some(s)
            }
            Err(e) => {
                out.push(f(Level::Fail, format!("{}: {e}", stack_file.display())));
                None
            }
        }
    } else {
        out.push(f(
            Level::Info,
            format!(
                "devstack {version}; no {} here, so only the machine is checked",
                stack_file.display()
            ),
        ));
        None
    };

    out.extend(docker(stack.as_ref().is_none_or(|s| !s.compose.is_empty())));
    if let Some(s) = &stack {
        out.push(tools(s));
        out.extend(ports(s, stack_file));
        if let Some(n) = crate::skills::outdated_note(stack_file) {
            out.push(f(Level::Warn, n.trim_start_matches("note: ").to_string()));
        }
    }
    out.push(hub());
    out.extend(release());
    out.extend(path_entry());
    #[cfg(windows)]
    out.extend(dev_drive(stack_file));
    out
}

fn docker(needed: bool) -> Vec<Finding> {
    let lvl = if needed { Level::Fail } else { Level::Info };
    match engine::status("docker") {
        Engine::Missing => vec![f(
            lvl,
            "Docker: no `docker` on PATH; install Docker Desktop (Windows, Linux) or Docker Engine (Linux)",
        )],
        Engine::Stopped { hint, starter } => vec![f(
            if needed { Level::Warn } else { Level::Info },
            if starter.is_some() {
                format!("Docker: the engine is not running; `devstack up` starts it ({hint})")
            } else {
                format!("Docker: the engine is not running; {hint}")
            },
        )],
        Engine::Running { version } => {
            let compose = Command::new("docker")
                .args(["compose", "version", "--short"])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .trim_start_matches('v')
                        .to_string()
                });
            match compose {
                None => vec![f(
                    lvl,
                    format!("Docker {version}, but no `docker compose` plugin"),
                )],
                Some(c) => match devstack_config::version_triple(&c) {
                    Some(v) if v < MIN_COMPOSE => vec![f(
                        lvl,
                        format!(
                            "Docker {version}, Compose {c}: stack files need Compose {}.{}.{} or newer (`!reset`, `!override`, `required: false`); update Docker",
                            MIN_COMPOSE.0, MIN_COMPOSE.1, MIN_COMPOSE.2
                        ),
                    )],
                    _ => vec![f(Level::Ok, format!("Docker {version}, Compose {c}"))],
                },
            }
        }
    }
}

/// The programs the stack file runs (first word of `build`, `run` and `exec`),
/// found on PATH or not.
fn tools(stack: &devstack_config::Stack) -> Finding {
    let mut names = BTreeSet::new();
    let mut add = |cmd: &str| {
        if let Some(p) = program(cmd) {
            names.insert(p);
        }
    };
    let platform = devstack_config::model::Platform::current();
    for s in stack.services.values() {
        if !s.platforms.is_empty() && !s.platforms.contains(&platform) {
            continue;
        }
        if let Some(b) = &s.build {
            add(b);
        }
        add(&s.run);
    }
    for c in stack.commands.values() {
        for step in &c.steps {
            if let Some(e) = &step.exec {
                add(e);
            }
        }
    }
    let missing: Vec<&String> = names.iter().filter(|n| find_on_path(n).is_none()).collect();
    if missing.is_empty() {
        f(
            Level::Ok,
            format!(
                "tools on PATH: {}",
                names.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        )
    } else {
        f(
            Level::Fail,
            format!(
                "not on PATH, but the stack file runs them: {} (install them, or open a new terminal after installing)",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    }
}

/// `dotnet build x.csproj` -> `dotnet`; placeholders and paths are skipped
/// (`dotnet {artifact}/X.dll` -> `dotnet`, `{artifact}/x` -> none).
fn program(cmd: &str) -> Option<String> {
    let first = cmd.split_whitespace().next()?.trim_matches(['"', '\'']);
    if first.contains(['{', '$', '/', '\\']) || first.is_empty() {
        return None;
    }
    Some(first.to_string())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(str::to_lowercase)
            .chain([String::new()])
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path).find_map(|dir| {
        exts.iter().find_map(|e| {
            let p = dir.join(format!("{name}{e}"));
            p.is_file().then_some(p)
        })
    })
}

fn ports(stack: &devstack_config::Stack, stack_file: &Path) -> Vec<Finding> {
    let running = crate::client::state_dir(stack_file)
        .ok()
        .and_then(|d| devstack_daemon::read_info(&d))
        .is_some();
    if running {
        let mut out = vec![f(Level::Ok, "ports: the stack runs, its daemon holds them")];
        // Containers healthy inside Docker, but not reachable from here.
        if let Ok(c) =
            crate::client::need(&crate::client::state_dir(stack_file).unwrap_or_default())
            && let Ok((200, body)) = c.call_raw("POST", "/v1/infra/check", None)
            && let Ok(lost) = serde_json::from_str::<Vec<serde_json::Value>>(&body)
        {
            for l in lost {
                out.push(f(
                    Level::Fail,
                    format!(
                        "container {}: {} accepts connections from this machine and closes them at once: Docker lost the port forward (restart the container: `docker compose restart {}` or the dashboard, else Docker Desktop), or nothing listens on it inside",
                        l["service"].as_str().unwrap_or(""),
                        l["ports"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).collect::<Vec<_>>().join(", "),
                        l["service"].as_str().unwrap_or("")
                    ),
                ));
            }
        }
        return out;
    }
    let taken: Vec<String> = stack
        .services
        .iter()
        .filter_map(|(n, s)| {
            let p = s.port?;
            devstack_daemon::supervisor::port_in_use(p).then(|| format!("{p} ({n})"))
        })
        .collect();
    if taken.is_empty() {
        vec![f(Level::Ok, "ports: free")]
    } else {
        vec![f(
            Level::Warn,
            format!(
                "ports in use by another process while the stack is down: {}; find the owner (`Get-NetTCPConnection -LocalPort <p>` / `ss -ltnp`)",
                taken.join(", ")
            ),
        )]
    }
}

/// A newer release, if GitHub answers within a few seconds (skipped with
/// `DEVSTACK_NO_UPDATE_CHECK`).
fn release() -> Option<Finding> {
    if devstack_hub::release::check_disabled() {
        return None;
    }
    let current = devstack_config::VERSION;
    Some(match devstack_hub::release::latest() {
        Ok(latest) if devstack_hub::release::newer(&latest, current) => f(
            Level::Warn,
            format!("devstack {latest} is out (this is {current}): `devstack update`"),
        ),
        Ok(_) => f(
            Level::Ok,
            format!("devstack {current} is the latest release"),
        ),
        Err(e) => f(
            Level::Info,
            format!("could not check for a newer release: {e}"),
        ),
    })
}

fn hub() -> Finding {
    let s = crate::integration::status();
    if s.autostart && s.link {
        f(
            Level::Ok,
            "hub: starts at logon, the dashboard can start it",
        )
    } else {
        f(
            Level::Info,
            format!(
                "hub: {}; `devstack hub install` registers both",
                crate::integration::status_text()
            ),
        )
    }
}

/// The running devstack should be the one a new terminal finds.
fn path_entry() -> Option<Finding> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| same_dir(&d, &dir)))
        .unwrap_or(false);
    let found = find_on_path("devstack");
    Some(match found {
        Some(p) if !same_dir(p.parent().unwrap_or(&p), &dir) => f(
            if crate::integration::is_dev_build(&exe) {
                Level::Info
            } else {
                Level::Warn
            },
            format!(
                "PATH finds another devstack first: {} (this one is {})",
                p.display(),
                exe.display()
            ),
        ),
        _ if on_path => f(Level::Ok, format!("on PATH: {}", exe.display())),
        _ => f(
            Level::Info,
            format!(
                "{} is not on PATH (fine for a build from source; the installer adds its directory)",
                exe.display()
            ),
        ),
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Builds and snapshots write many small files; Defender scans each one. A Dev
/// Drive (or an exclusion, which needs admin rights) makes that much cheaper.
#[cfg(windows)]
fn dev_drive(stack_file: &Path) -> Option<Finding> {
    let abs = std::path::absolute(stack_file).ok()?;
    let drive: String = abs.to_string_lossy().chars().take(2).collect();
    if !drive.ends_with(':') {
        return None;
    }
    let out = Command::new("fsutil")
        .args(["devdrv", "query", &drive])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    // "This is a trusted developer volume." / "Dies ist ein vertrauenswürdiges Entwicklervolume."
    let trusted = out.status.success()
        && (text.contains("trusted developer volume") || text.contains("vertrauensw"));
    Some(if trusted {
        f(Level::Ok, format!("{drive} is a trusted Dev Drive"))
    } else {
        f(
            Level::Info,
            format!(
                "{drive} is not a Dev Drive: Defender scans every file a build or snapshot writes. A Dev Drive, or an exclusion for {} (admin: `Add-MpPreference -ExclusionPath <dir>`), makes builds faster",
                abs.parent().unwrap_or(&abs).join(".devstack").display()
            ),
        )
    })
}

pub fn print(findings: &[Finding]) -> bool {
    for x in findings {
        let tag = match x.level {
            Level::Ok => "ok  ",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        println!("{tag}  {}", x.text);
    }
    findings.iter().all(|x| x.level != Level::Fail)
}

pub fn text(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|x| {
            let tag = match x.level {
                Level::Ok => "ok",
                Level::Info => "info",
                Level::Warn => "warn",
                Level::Fail => "FAIL",
            };
            format!("{tag}: {}", x.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_of_a_command_line() {
        assert_eq!(
            program("dotnet build x.csproj -c Debug").as_deref(),
            Some("dotnet")
        );
        assert_eq!(
            program("dotnet {artifact}/Api.dll").as_deref(),
            Some("dotnet")
        );
        assert_eq!(program("\"npm\" run serve").as_deref(), Some("npm"));
        assert_eq!(program("{artifact}/api").as_deref(), None);
        assert_eq!(program("./scripts/x.sh").as_deref(), None);
        assert_eq!(program("${NODE} x.js").as_deref(), None);
    }

    #[test]
    fn a_tool_that_does_not_exist_is_missing() {
        assert!(find_on_path("devstack-no-such-tool-x9").is_none());
    }
}
