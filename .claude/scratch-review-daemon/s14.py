import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'diag.rs',[
("""//! With `DEVSTACK_OTLP_ENDPOINT` (e.g. `http://127.0.0.1:4318`) every line is
//! also sent as an OTLP log record (HTTP/JSON, `/v1/logs`), batched, as
//! `service.name` `devstack-daemon` or `devstack-hub`. Optional and lossy by
//! design: a collector that is down costs nothing but the records.""",
"""//! With `DEVSTACK_OTLP_ENDPOINT` (e.g. `http://127.0.0.1:4318`) every line is
//! also sent as an OTLP log record (HTTP/JSON, `/v1/logs`), batched, as
//! `service.name` `devstack-daemon` or `devstack-hub`. Optional and lossy by
//! design: a collector that is down costs nothing but the records.
//!
//! What leaves the machine (that collector, a pool's, ADR-0037) carries no
//! service output: a reason or error of several lines is devstack's own first
//! line, then the output of a build, a job or a service (which may hold a
//! connection string); only the first line goes out. The file keeps all."""),
("""        crate::telemetry::record(&line);
        if let Some(tx) = OTLP.get().and_then(Option::as_ref) {
            let _ = tx.try_send(Value::Object(line.clone()));
        }""",
"""        let otlp = OTLP.get().and_then(Option::as_ref);
        if otlp.is_some() || crate::telemetry::active() {
            let out = outbound(&line);
            crate::telemetry::record(&out);
            if let Some(tx) = otlp {
                let _ = tx.try_send(Value::Object(out));
            }
        }"""),
("""fn open_append(path: &Path) -> Option<File> {""",
"""/// A line as it may leave the machine: every text cut to its first line
/// (ADR-0037: never service output).
pub fn outbound(line: &Map<String, Value>) -> Map<String, Value> {
    fn cut(v: &Value) -> Value {
        match v {
            Value::String(s) => match s.split_once('\\n') {
                Some((first, _)) => Value::String(format!("{} …", first.trim_end())),
                None => v.clone(),
            },
            Value::Array(a) => Value::Array(a.iter().map(cut).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), cut(v))).collect()),
            other => other.clone(),
        }
    }
    line.iter().map(|(k, v)| (k.clone(), cut(v))).collect()
}

fn open_append(path: &Path) -> Option<File> {"""),
("""pub fn error(event: &str, fields: Value) {
    log("error", event, fields);
}""",
"""pub fn error(event: &str, fields: Value) {
    log("error", event, fields);
}

/// A warning that may repeat many times a second (an accept loop that
/// fails, a tunnel per connection): at most one line per `key` every ten
/// seconds, with how many were left out since (`suppressed`).
pub fn warn_limited(key: &str, event: &str, fields: Value) {
    static LAST: Mutex<Option<std::collections::HashMap<String, (std::time::Instant, u64)>>> =
        Mutex::new(None);
    let suppressed = {
        let mut last = LAST.lock().unwrap();
        let map = last.get_or_insert_with(Default::default);
        match map.get_mut(key) {
            Some((at, n)) if at.elapsed() < std::time::Duration::from_secs(10) => {
                *n += 1;
                return;
            }
            Some((at, n)) => {
                *at = std::time::Instant::now();
                std::mem::take(n)
            }
            None => {
                map.insert(key.to_string(), (std::time::Instant::now(), 0));
                0
            }
        }
    };
    let mut fields = fields;
    if suppressed > 0
        && let Value::Object(f) = &mut fields
    {
        f.insert("suppressed".into(), suppressed.into());
    }
    warn(event, fields);
}"""),
])
apply(W+'telemetry.rs',[
("""/// Keep a diagnostic line for the next send (when a pool takes them).
pub fn record(""",
"""/// Whether a pool takes the diagnostic lines now.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Keep a diagnostic line for the next send (when a pool takes them).
pub fn record("""),
])
apply(W+'tcpfwd.rs',[
("""                crate::diag::warn(
                    "forward_accept_failed",
                    serde_json::json!({ "port": port, "error": e.to_string() }),
                );""",
"""                crate::diag::warn_limited(
                    &format!("forward_accept_failed:{port}"),
                    "forward_accept_failed",
                    serde_json::json!({ "port": port, "error": e.to_string() }),
                );"""),
])
print("ok")
