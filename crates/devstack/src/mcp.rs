//! `devstack mcp`: an MCP server on stdio for coding agents (plan §2.2).
//!
//! A client of the daemon's API like the CLI (ADR-0007): every tool maps to one
//! endpoint, and nothing here starts, stops or builds by itself. The protocol
//! is JSON-RPC 2.0, one message per line; stdout carries nothing else.

use crate::client::{self, Client, UpOutcome};
use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "devstack runs this repository's development stack: native \
services built from the working tree, plus the infrastructure from the project's Compose \
files. Start with `status`. `restart_service` rebuilds the service from the current files and \
replaces the running instance only if the build succeeds; compiler errors come back in the \
result, and the old instance keeps running. `logs` shows a service's console output. \
Changes to watched files are built and swapped in by devstack itself: after an edit, `status` \
shows `not yet active: building | starting | failed | held` until the change runs, and the \
old build answers meanwhile; do not test the change before that line is gone. A `held` \
candidate (a schema change) needs the user's go-ahead before `promote_service`. Commands \
(`run_command`) are scripts the stack file declares, such as a database reset; \
some ask for confirmation, which must come from the user.";

type Out = Arc<Mutex<std::io::Stdout>>;

pub fn serve(stack_file: &Path) -> Result<()> {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let file = std::path::absolute(stack_file)?;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => batch.into_iter().for_each(|m| handle(m, &file, &out)),
            Ok(m) => handle(m, &file, &out),
            Err(e) => send(
                &out,
                error(Value::Null, -32700, &format!("parse error: {e}")),
            ),
        }
    }
    Ok(()) // the client closed stdin: end of session
}

fn send(out: &Out, msg: Value) {
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(o, "{msg}");
    let _ = o.flush();
}

fn reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn handle(m: Value, file: &Path, out: &Out) {
    let id = m.get("id").cloned();
    let Some(method) = m.get("method").and_then(Value::as_str) else {
        return; // a response; this server sends no requests
    };
    let Some(id) = id else {
        return; // a notification (initialized, cancelled, …)
    };
    let params = m.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&VERSIONS[0]);
            send(
                out,
                reply(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "devstack", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS,
                    }),
                ),
            );
        }
        "ping" => send(out, reply(id, json!({}))),
        "tools/list" => send(out, reply(id, json!({ "tools": tools() }))),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            if !tools().iter().any(|t| t["name"] == name) {
                send(out, error(id, -32602, &format!("unknown tool `{name}`")));
                return;
            }
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            // Builds and restarts take minutes: each call on its own thread,
            // so `ping` and other calls are answered meanwhile.
            let (file, out) = (file.to_path_buf(), out.clone());
            std::thread::spawn(move || {
                let (text, is_error) = match call(&name, &args, &file) {
                    Ok(t) => (t, false),
                    Err(e) => (format!("{e:#}"), true),
                };
                send(
                    &out,
                    reply(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": text }],
                            "isError": is_error,
                        }),
                    ),
                );
            });
        }
        other => send(out, error(id, -32601, &format!("unknown method `{other}`"))),
    }
}

fn tool(name: &str, description: &str, props: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false,
        },
        "annotations": { "readOnlyHint": read_only },
    })
}

pub fn tools() -> Vec<Value> {
    let service =
        json!({ "service": { "type": "string", "description": "Service name from `status`" } });
    vec![
        tool(
            "status",
            "State of every service (build id, port, pid), the infrastructure containers and the commands the stack offers. GET /v1/stack.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "up",
            "Start the stack: the daemon if needed, the infrastructure, then every autostart service, building what is not built yet. Waits until all are ready or one fails. POST /v1/up.",
            json!({ "timeout_s": { "type": "integer", "minimum": 1, "description": "Give up waiting after this many seconds (default 600); the stack keeps starting" } }),
            &[],
            false,
        ),
        tool(
            "down",
            "Stop every service; with `infra`, also stop and remove the infrastructure containers (volumes are kept). The daemon keeps running. POST /v1/down.",
            json!({ "infra": { "type": "boolean", "default": false } }),
            &[],
            false,
        ),
        tool(
            "start_service",
            "Build the service if needed and start it. A group name starts every service of the group. POST /v1/services/{service}/start.",
            service.clone(),
            &["service"],
            false,
        ),
        tool(
            "stop_service",
            "Stop the service; a group name stops every service of the group. POST /v1/services/{service}/stop.",
            service.clone(),
            &["service"],
            false,
        ),
        tool(
            "restart_service",
            "Rebuild the service from the current working tree and replace the running instance. If the build fails, the old instance keeps running and the compiler output is returned. A group name restarts every service of the group (built at the same time) and its containers. POST /v1/services/{service}/restart.",
            service.clone(),
            &["service"],
            false,
        ),
        tool(
            "hot_reload",
            "Switch a built service into hot-reload mode (`on`: the toolchain's own watcher, e.g. `dotnet watch`, runs from the source tree on the same port; edits apply in place, but a broken edit is no longer kept away from the running service) or back to builds and candidates (`off`). Needs `hot_reload` for the service in the stack file. POST /v1/services/{service}/hot-reload-on|off.",
            json!({
                "service": service["service"],
                "on": { "type": "boolean" },
            }),
            &["service", "on"],
            false,
        ),
        tool(
            "promote_service",
            "Take over a candidate that waits for promotion, e.g. because it changes the database schema: runs the jobs the service comes after (a migrator), then switches. Ask the user first; `status` shows what waits and why. POST /v1/services/{service}/promote.",
            service.clone(),
            &["service"],
            false,
        ),
        tool(
            "rollback_service",
            "Go back to the build that ran before the last replacement, without building and without running jobs (a schema migration stays). POST /v1/services/{service}/rollback.",
            service.clone(),
            &["service"],
            false,
        ),
        tool(
            "logs",
            "Console output of the service's current instance, or of earlier ones (`build`, `since`, `file`, `list`: an instance that crashed or was replaced). A Compose service name gives that container's output (timestamped). With `grep` only matching lines; without `service` but with `grep`, every service is searched (\"where did this error come from\"). GET /v1/services/{service}/logs, GET /v1/logs.",
            json!({
                "service": service["service"],
                "tail": { "type": "integer", "minimum": 1, "default": 200, "description": "Number of last (matching) lines, per service when searching all" },
                "grep": { "type": "string", "description": "Only lines containing this (case-insensitive)" },
                "build": { "type": "string", "description": "Instead of the current instance: the build log of this build and the logs of every instance that ran it (an id prefix is enough)" },
                "since": { "type": "string", "description": "Instead of the current instance: every instance's log written to within this time, e.g. `30m` (lines carry no time of their own). Also for the search across all services" },
                "file": { "type": "string", "description": "One kept log by its file name (from `list`)" },
                "list": { "type": "boolean", "description": "List the service's kept logs (instances and builds, newest first) instead of reading one" },
            }),
            &[],
            true,
        ),
        tool(
            "builds",
            "The builds of a service, newest first, from devstack's diagnostic log: which went live, which failed (with the first line of the reason), which a newer change replaced, which wait for promote, and which runs now. For \"did my change ever go live\" and \"since when is it broken\". GET /v1/services/{service}/builds.",
            json!({
                "service": service["service"],
                "limit": { "type": "integer", "minimum": 1, "default": 20 },
            }),
            &["service"],
            true,
        ),
        tool(
            "doctor",
            "Is this machine ready for the stack: Docker engine and Compose version, the tools the stack file runs, ports, the hub, PATH. Read-only. Local file and process checks, no daemon needed. Run it before guessing why something does not start.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "diagnostics",
            "devstack's own log: every decision of the daemon (service and candidate states with reasons, requests that changed something, start and stop) or of the dashboard hub (Docker, pairing, stack start/stop). The place to look when something happened and the service log does not say why. Local file, works with the daemon stopped.",
            json!({
                "source": { "type": "string", "enum": ["daemon", "hub"], "default": "daemon" },
                "tail": { "type": "integer", "minimum": 1, "default": 100, "description": "Number of last lines" },
                "grep": { "type": "string", "description": "Only lines containing this (case-insensitive), e.g. a service name or `warn`" },
            }),
            &[],
            true,
        ),
        tool(
            "run_command",
            "Run a command the stack file declares (see `status`), wait for it and return its output. A command with a confirmation text only runs with `confirm: true`; set it only after the user agreed to that text. POST /v1/commands/{command}.",
            json!({
                "command": { "type": "string" },
                "confirm": { "type": "boolean", "default": false, "description": "The user agreed to the command's confirmation text" },
            }),
            &["command"],
            false,
        ),
        tool(
            "hub_status",
            "Is the dashboard hub running (it serves the dashboard for every stack on a fixed port)? Local check, no daemon needed.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "hub_start",
            "Start the dashboard hub if it is not running and return a one-time pairing link (valid 60 s) for the user to open the dashboard in a browser. POST /hub/pair.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "usage",
            "CPU, memory and disk per service, build job, container, group and lane. GET /v1/usage.",
            json!({}),
            &[],
            true,
        ),
    ]
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing argument `{key}`"))
}

/// Path segments come from the agent: refuse anything that is not a plain name.
fn name_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let v = arg_str(args, key)?;
    if !v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("`{key}` must be a plain name, got `{v}`");
    }
    Ok(v)
}

fn connect(file: &Path) -> Result<Client> {
    client::need(&client::state_dir(file)?)
        .map_err(|_| anyhow::anyhow!("no daemon is running for this stack; call `up` first"))
}

fn checked(status: u16, body: String) -> Result<String> {
    if status >= 400 {
        anyhow::bail!("{}", client::error_text(&body));
    }
    Ok(body)
}

fn call(name: &str, args: &Value, file: &Path) -> Result<String> {
    match name {
        "status" => {
            let mut text = client::status_text(&connect(file)?.stack()?);
            if let Some(n) = crate::skills::outdated_note(file) {
                text.push_str(&format!("\n{n}\n"));
            }
            Ok(text)
        }
        "up" => {
            let timeout = Duration::from_secs(args["timeout_s"].as_u64().unwrap_or(600));
            let c = client::ensure_daemon(file, &client::state_dir(file)?)?;
            match client::start_and_wait(&c, file, Some(timeout), |_, _| {})? {
                UpOutcome::Ready(stack) => Ok(format!("ready\n\n{}", client::status_text(&stack))),
                UpOutcome::Failed { service, reason } => {
                    anyhow::bail!("{service} failed:\n{reason}")
                }
                UpOutcome::TimedOut(stack) => anyhow::bail!(
                    "still starting after {} s (it continues; check `status`)\n\n{}",
                    timeout.as_secs(),
                    client::status_text(&stack)
                ),
            }
        }
        "down" => {
            let infra = args["infra"].as_bool().unwrap_or(false);
            let (status, body) = connect(file)?.post(&format!("/v1/down?infra={infra}"), None)?;
            checked(status, body)?;
            Ok(if infra {
                "services stopped, infrastructure removed".into()
            } else {
                "services stopped; infrastructure keeps running".into()
            })
        }
        "start_service" | "stop_service" | "restart_service" | "promote_service"
        | "rollback_service" => {
            let action = name.trim_end_matches("_service");
            let svc = name_arg(args, "service")?;
            let (status, body) =
                connect(file)?.post(&format!("/v1/services/{svc}/{action}"), None)?;
            let body = checked(status, body)?;
            Ok(client::status_text(&serde_json::from_str(&body)?))
        }
        "logs" => {
            let tail = args["tail"].as_u64().unwrap_or(200);
            let grep = args["grep"].as_str().filter(|g| !g.is_empty());
            let filter = client::LogFilter {
                tail: tail as usize,
                grep,
                build: args["build"].as_str(),
                since: args["since"].as_str(),
                file: args["file"].as_str(),
            };
            let q = filter.query();
            if args["list"].as_bool() == Some(true) {
                let svc = name_arg(args, "service")?;
                return connect(file)?.get(&format!("/v1/services/{svc}/logs/files"));
            }
            if args["service"].is_null() {
                let Some(g) = grep else {
                    anyhow::bail!("name a `service`, or search every service with `grep`");
                };
                let text = connect(file)?.get(&format!("/v1/logs?{q}"))?;
                return Ok(if text.is_empty() {
                    format!("no service log contains `{g}`")
                } else {
                    text
                });
            }
            let svc = name_arg(args, "service")?;
            let text = connect(file)?.get(&format!("/v1/services/{svc}/logs?{q}"))?;
            Ok(if text.is_empty() {
                format!("`{svc}` has written nothing yet (or nothing matching)")
            } else {
                text
            })
        }
        "builds" => {
            let svc = name_arg(args, "service")?;
            let limit = args["limit"].as_u64().unwrap_or(20);
            let list: Vec<Value> = serde_json::from_str(
                &connect(file)?.get(&format!("/v1/services/{svc}/builds?limit={limit}"))?,
            )?;
            Ok(client::builds_text(&list))
        }
        "hot_reload" => {
            let svc = name_arg(args, "service")?;
            let on = args["on"]
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("`on` (true or false) is required"))?;
            let action = if on {
                "hot-reload-on"
            } else {
                "hot-reload-off"
            };
            let (status, body) =
                connect(file)?.post(&format!("/v1/services/{svc}/{action}"), None)?;
            let body = checked(status, body)?;
            Ok(client::status_text(&serde_json::from_str(&body)?))
        }
        "doctor" => Ok(crate::doctor::text(&crate::doctor::run(file))),
        "diagnostics" => {
            let path = if args["source"].as_str() == Some("hub") {
                devstack_hub::log_path()
            } else {
                client::state_dir(file)?.join("daemon.log")
            };
            let tail = args["tail"].as_u64().unwrap_or(100) as usize;
            let lines = devstack_daemon::diag::tail(&path, tail, args["grep"].as_str());
            Ok(if lines.is_empty() {
                format!("nothing in {}", path.display())
            } else {
                lines
                    .iter()
                    .map(|l| devstack_daemon::diag::pretty(l))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        "run_command" => run_command(file, name_arg(args, "command")?, args),
        "hub_status" => Ok(crate::hubcmd::status_text()),
        "hub_start" => {
            let url = crate::hubcmd::pairing_url()?;
            Ok(format!(
                "{}
pairing link for the user (single use, 60 s): {url}",
                crate::hubcmd::status_text()
            ))
        }
        "usage" => {
            let report: Value = serde_json::from_str(&connect(file)?.get("/v1/usage")?)?;
            Ok(crate::topcmd::render(&report))
        }
        other => anyhow::bail!("unknown tool `{other}`"),
    }
}

fn run_command(file: &Path, cmd: &str, args: &Value) -> Result<String> {
    let c = connect(file)?;
    let confirm = args["confirm"].as_bool().unwrap_or(false);
    let (status, body) = c.post(
        &format!("/v1/commands/{cmd}"),
        Some(json!({ "confirm": confirm })),
    )?;
    if status == 428 {
        let text = serde_json::from_str::<Value>(&body)?["confirm"]
            .as_str()
            .unwrap_or("Continue?")
            .to_string();
        anyhow::bail!(
            "`{cmd}` asks for confirmation: \"{text}\". Ask the user; call again with \
             `confirm: true` only if they agree."
        );
    }
    let body = checked(status, body)?;
    let run = serde_json::from_str::<Value>(&body)?["run"]
        .as_u64()
        .unwrap_or(0);
    let t0 = Instant::now();
    loop {
        let info: Value = serde_json::from_str(&c.get(&format!("/v1/runs/{run}"))?)?;
        if info["status"] != "running" {
            let log = c.get(&format!("/v1/runs/{run}/log"))?;
            if info["status"] == "succeeded" {
                return Ok(log);
            }
            anyhow::bail!(
                "`{cmd}` failed: {}\n\n{log}",
                info["error"].as_str().unwrap_or("")
            );
        }
        if t0.elapsed() > Duration::from_secs(1800) {
            anyhow::bail!("`{cmd}` still runs after 30 minutes (run {run})");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_an_object_schema_and_a_unique_name() {
        let tools = tools();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        for t in &tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            // Daemon tools name their endpoint; the hub tools are local checks.
            let local = t["name"].as_str().unwrap().starts_with("hub_")
                || t["description"].as_str().unwrap().contains("Local file");
            assert!(
                local || t["description"].as_str().unwrap().contains("/v1/"),
                "names its endpoint: {t}"
            );
        }
    }

    #[test]
    fn names_from_the_agent_stay_plain() {
        assert!(name_arg(&json!({ "service": "api" }), "service").is_ok());
        for bad in ["../x", "a/b", "a?b", "", "a b"] {
            assert!(
                name_arg(&json!({ "service": bad }), "service").is_err(),
                "{bad}"
            );
        }
    }
}
