//! MCP upstreams that are a local command (ADR-0069; card on #316, point 3):
//! the hub starts the command the user entered, as the user, with a small
//! base environment to run at all and the variables the user set for it,
//! and speaks JSON-RPC with it over its stdin and stdout, one message per
//! line. One process per upstream, started when it is first asked and again
//! when it ended; its stderr goes to a log beside the hub's.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// The variables of the hub's own environment a command gets besides the
/// ones the user set: what a program needs to start and find its tools.
/// Nothing else of the hub's environment (its tokens, its settings) goes.
const BASE: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "OS",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "XDG_RUNTIME_DIR",
];

/// A running command of one upstream.
pub struct Process {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    waiting: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
}

impl Drop for Process {
    fn drop(&mut self) {
        let mut c = self.child.lock().unwrap();
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// `program` on PATH, with the extensions of PATHEXT on Windows (`npx` is
/// `npx.cmd` there); a path is taken as it is.
fn resolve(program: &str) -> PathBuf {
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        return p.to_path_buf();
    }
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
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path).find_map(|dir| {
                exts.iter().find_map(|e| {
                    let c = dir.join(format!("{program}{e}"));
                    c.is_file().then_some(c)
                })
            })
        })
        .unwrap_or_else(|| p.to_path_buf())
}

impl Process {
    /// Start `command` with `env` on top of [`BASE`], its stderr to `log`,
    /// and open the MCP session (`initialize`).
    pub fn start(
        command: &[String],
        env: &BTreeMap<String, String>,
        log: &Path,
    ) -> Result<Self, String> {
        let (program, args) = command
            .split_first()
            .ok_or("an upstream command names no program")?;
        let mut cmd = Command::new(resolve(program));
        cmd.args(args).env_clear();
        for k in BASE {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        cmd.envs(env);
        if let Some(d) = log.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let err = std::fs::File::create(log)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(err);
        devstack_desktop::no_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("`{program}` did not start: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let waiting: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>> = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let p = Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            waiting: waiting.clone(),
            next: AtomicU64::new(1),
            alive: alive.clone(),
        };
        let (w, a) = (waiting, alive);
        std::thread::Builder::new()
            .name("mcp-upstream-stdio".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    // Anything that is not a JSON-RPC answer (a banner, a
                    // notification, a request of the server's) is passed by.
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let Some(id) = v["id"].as_u64() else { continue };
                    if v.get("method").is_some() {
                        continue;
                    }
                    if let Some(tx) = w.lock().unwrap().remove(&id) {
                        let _ = tx.send(v);
                    }
                }
                a.store(false, Ordering::SeqCst);
                // Whoever waits hears that it ended.
                w.lock().unwrap().clear();
            })
            .map_err(|e| e.to_string())?;
        let init = p.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "devstack-hub", "version": env!("CARGO_PKG_VERSION") },
            }),
            Duration::from_secs(60),
        )?;
        let _ = init;
        p.write(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(p)
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn write(&self, msg: &Value) -> Result<(), String> {
        let mut s = self.stdin.lock().unwrap();
        writeln!(s, "{msg}")
            .and_then(|()| s.flush())
            .map_err(|e| format!("the upstream's command took nothing more: {e}"))
    }

    /// `method` with `params`: its `result`, within `within`.
    pub fn request(&self, method: &str, params: Value, within: Duration) -> Result<Value, String> {
        if !self.alive() {
            return Err("the upstream's command has ended".into());
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        if let Err(e) =
            self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
        {
            self.waiting.lock().unwrap().remove(&id);
            return Err(e);
        }
        let a = rx.recv_timeout(within).map_err(|e| {
            self.waiting.lock().unwrap().remove(&id);
            match e {
                mpsc::RecvTimeoutError::Timeout => {
                    format!("no answer to `{method}` within {} s", within.as_secs())
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    "the upstream's command has ended".to_string()
                }
            }
        })?;
        if let Some(e) = a.get("error") {
            return Err(format!(
                "`{method}`: {}",
                e["message"].as_str().unwrap_or("an error")
            ));
        }
        Ok(a.get("result").cloned().unwrap_or(Value::Null))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The test binary itself as a stdio MCP server: this test, run with
    /// `DEVSTACK_FAKE_STDIO_MCP`, answers JSON-RPC on stdin (the harness's
    /// own lines on stdout are passed by, as a banner would be). It echoes
    /// the variables it was given, to show which reached it.
    #[test]
    fn fake_stdio_mcp_server() {
        if std::env::var_os("DEVSTACK_FAKE_STDIO_MCP").is_none() {
            return;
        }
        let stdin = std::io::stdin();
        let mut out = std::io::stdout();
        // The harness wrote `test … ... ` without a line end.
        let _ = writeln!(out);
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let Ok(m) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(id) = m.get("id").cloned() else {
                continue;
            };
            let result = match m["method"].as_str().unwrap_or("") {
                "initialize" => json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
                "tools/list" => {
                    json!({ "tools": [{ "name": "env", "description": "What it was given", "inputSchema": { "type": "object" } }] })
                }
                "tools/call" => {
                    let k = m["params"]["arguments"]["var"].as_str().unwrap_or("");
                    json!({ "content": [{ "type": "text", "text": std::env::var(k).unwrap_or_else(|_| "<unset>".into()) }] })
                }
                _ => json!({}),
            };
            let _ = writeln!(
                out,
                "{}",
                json!({ "jsonrpc": "2.0", "id": id, "result": result })
            );
            let _ = out.flush();
        }
        std::process::exit(0);
    }

    /// The test binary, started to run only `fake_stdio_mcp_server`.
    pub fn fake_command() -> Vec<String> {
        vec![
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--exact".into(),
            "stdio::tests::fake_stdio_mcp_server".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ]
    }

    #[test]
    fn a_command_is_asked_over_its_stdio_with_only_the_environment_it_was_given() {
        // SAFETY of the test: a variable set for this process, which the
        // command must not see.
        let leak = format!("DEVSTACK_TEST_LEAK_{}", std::process::id());
        // Set in the hub's environment: not passed on.
        unsafe { std::env::set_var(&leak, "hub-secret") };
        let d = tempfile::tempdir().unwrap();
        let env = BTreeMap::from([
            ("DEVSTACK_FAKE_STDIO_MCP".to_string(), "1".to_string()),
            ("GIVEN".to_string(), "for-the-command".to_string()),
        ]);
        let p = Process::start(&fake_command(), &env, &d.path().join("log")).unwrap();
        let tools = p
            .request("tools/list", json!({}), Duration::from_secs(30))
            .unwrap();
        assert_eq!(tools["tools"][0]["name"], "env");
        let ask = |var: &str| {
            p.request(
                "tools/call",
                json!({ "name": "env", "arguments": { "var": var } }),
                Duration::from_secs(30),
            )
            .unwrap()["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(ask("GIVEN"), "for-the-command");
        assert_eq!(ask(&leak), "<unset>");
        assert_ne!(ask("PATH"), "<unset>", "the base environment goes");
        drop(p);
    }
}
