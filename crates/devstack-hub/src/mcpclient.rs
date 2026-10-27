//! A client of an MCP server over Streamable HTTP, as far as the hub needs
//! one for its upstreams (ADR-0069): `initialize` once per session, then
//! `tools/list` and `tools/call`. A request is a JSON-RPC POST; the answer is
//! JSON, or a server-sent event stream whose event with the request's id is
//! the answer. The session id the server gives comes with every later
//! request; a server that forgot it (`404`) gets a new session once.

use serde_json::{Value, json};

/// The protocol version asked for; the server's answer is used after.
const PROTOCOL: &str = "2025-06-18";

/// A session with one upstream.
#[derive(Debug, Clone, Default)]
pub struct Session {
    id: Option<String>,
    version: Option<String>,
    ready: bool,
}

#[derive(Debug)]
pub enum Error {
    /// The upstream refused the credential (`401`, `403`).
    Unauthorized,
    /// Anything else: not reachable, an HTTP or a JSON-RPC error.
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized => write!(f, "the upstream refused the login"),
            Error::Other(e) => write!(f, "{e}"),
        }
    }
}

/// `method` with `params` on the upstream at `url`: its `result`. Starts the
/// session first when it has none.
pub fn request(
    http: &ureq::Agent,
    url: &str,
    bearer: Option<&str>,
    session: &mut Session,
    method: &str,
    params: Value,
) -> Result<Value, Error> {
    for attempt in 0..2 {
        if !session.ready {
            start(http, url, bearer, session)?;
        }
        match post(http, url, bearer, session, method, Some(params.clone()), 2) {
            Err(Gone) if attempt == 0 => {
                *session = Session::default();
            }
            Err(Gone) => {
                return Err(Error::Other(
                    "the upstream keeps forgetting its session".into(),
                ));
            }
            Ok(r) => return r,
        }
    }
    unreachable!("the loop returns")
}

/// The server forgot the session (`404` with one).
struct Gone;

fn start(
    http: &ureq::Agent,
    url: &str,
    bearer: Option<&str>,
    session: &mut Session,
) -> Result<(), Error> {
    *session = Session::default();
    let params = json!({
        "protocolVersion": PROTOCOL,
        "capabilities": {},
        "clientInfo": { "name": "devstack-hub", "version": env!("CARGO_PKG_VERSION") },
    });
    let result = match post(http, url, bearer, session, "initialize", Some(params), 1) {
        Ok(r) => r?,
        Err(Gone) => {
            return Err(Error::Other(
                "the upstream refused to start a session".into(),
            ));
        }
    };
    session.version = result["protocolVersion"].as_str().map(str::to_string);
    // The notification has no answer to wait for (`202`).
    let _ = notify(http, url, bearer, session, "notifications/initialized");
    session.ready = true;
    Ok(())
}

fn with_headers(
    mut req: ureq::RequestBuilder<ureq::typestate::WithBody>,
    bearer: Option<&str>,
    session: &Session,
) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
    req = req
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(b) = bearer {
        req = req.header("Authorization", &format!("Bearer {b}"));
    }
    if let Some(id) = &session.id {
        req = req.header("Mcp-Session-Id", id);
    }
    if let Some(v) = &session.version {
        req = req.header("MCP-Protocol-Version", v);
    }
    req
}

fn notify(
    http: &ureq::Agent,
    url: &str,
    bearer: Option<&str>,
    session: &Session,
    method: &str,
) -> Result<(), String> {
    let body = json!({ "jsonrpc": "2.0", "method": method });
    with_headers(http.post(url), bearer, session)
        .send(body.to_string())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// One request: `Err(Gone)` when the server forgot the session; else its
/// result or why not.
fn post(
    http: &ureq::Agent,
    url: &str,
    bearer: Option<&str>,
    session: &mut Session,
    method: &str,
    params: Option<Value>,
    id: u64,
) -> Result<Result<Value, Error>, Gone> {
    let mut body = json!({ "jsonrpc": "2.0", "id": id, "method": method });
    if let Some(p) = params {
        body["params"] = p;
    }
    let mut r = match with_headers(http.post(url), bearer, session).send(body.to_string()) {
        Ok(r) => r,
        Err(e) => return Ok(Err(Error::Other(format!("{url}: {e}")))),
    };
    let status = r.status().as_u16();
    if status == 404 && session.id.is_some() {
        return Err(Gone);
    }
    if status == 401 || status == 403 {
        return Ok(Err(Error::Unauthorized));
    }
    if let Some(s) = r
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        session.id = Some(s.to_string());
    }
    let sse = r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/event-stream"));
    let text = match r.body_mut().read_to_string() {
        Ok(t) => t,
        Err(e) => return Ok(Err(Error::Other(format!("{url}: {e}")))),
    };
    if !(200..300).contains(&status) {
        return Ok(Err(Error::Other(format!(
            "{url} answered {status}: {}",
            text.trim()
        ))));
    }
    let answer = if sse {
        from_events(&text, id)
    } else {
        serde_json::from_str(&text).ok()
    };
    let Some(a) = answer else {
        return Ok(Err(Error::Other(format!(
            "{url} gave no answer to `{method}`"
        ))));
    };
    if let Some(e) = a.get("error") {
        return Ok(Err(Error::Other(format!(
            "`{method}`: {}",
            e["message"].as_str().unwrap_or("an error")
        ))));
    }
    Ok(Ok(a.get("result").cloned().unwrap_or(Value::Null)))
}

/// The JSON-RPC answer with `id` in a server-sent event stream.
fn from_events(text: &str, id: u64) -> Option<Value> {
    let mut data = String::new();
    let mut found = None;
    for line in text.lines().chain(std::iter::once("")) {
        if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        } else if line.is_empty() && !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(&data)
                && v["id"] == json!(id)
            {
                found = Some(v);
            }
            data.clear();
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_found_among_events() {
        let text = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":2,\n\
                    data: \"result\":{\"tools\":[]}}\n\n";
        let v = from_events(text, 2).unwrap();
        assert_eq!(v["result"]["tools"], json!([]));
        assert!(from_events(text, 3).is_none());
    }
}
