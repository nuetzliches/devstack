//! Filters on an upstream tool's result (ADR-0069, addendum 2026-11-10): a
//! local command the hub runs on fields of a result before `devstack mcp`
//! gets it, such as `veilq mark -` on a ticket's description, so that an
//! agent never reads what the command takes out.
//!
//! A filter names the upstream, the tool, the paths of the fields in the
//! result's JSON and the command. The command gets a field's text on stdin
//! and answers JSON with `text` (what the agent reads) and, optionally,
//! `write` (what to store back through the upstream). It fails closed: a
//! command missing, failing or answering something else makes the tool call
//! fail; the raw field is never handed on.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How long a filter's command may take for one field: veilq with its
/// name model needs seconds for a long body.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// At most this much of a command's output is read.
const OUTPUT_MAX: usize = 16 * 1024 * 1024;

/// At most this many filters, and paths in one.
const FILTERS_MAX: usize = 64;
const PATHS_MAX: usize = 16;

/// One filter as the user configured it (`[[filter]]` in `upstreams.toml`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    /// The upstream's name: the user's own, or `<pool>:<name>` of a pool's
    /// they accepted.
    pub upstream: String,
    /// The tool's name at the upstream (without the prefix).
    pub tool: String,
    /// Where the fields are in the result's JSON: names joined by `.`, `[*]`
    /// for every item of an array (`description`, `issues[*].description`).
    pub paths: Vec<String>,
    /// The program and its arguments; it reads the field on stdin.
    pub command: Vec<String>,
    /// What to store back when the command answers with `write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<WriteBack>,
}

/// A write-back: a tool of the same upstream, and its arguments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WriteBack {
    pub tool: String,
    /// Each a text as it is, `$write` (the command's `write`), or
    /// `$args.<name>` (that argument of the call that was filtered).
    pub arguments: BTreeMap<String, String>,
}

/// One step of a path.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Key(String),
    Each,
}

/// The steps of `path`, or why it is none.
fn parse(path: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    if path.trim().is_empty() {
        return Err("a path is empty".into());
    }
    for part in path.split('.') {
        let (key, rest) = match part.find('[') {
            Some(i) => part.split_at(i),
            None => (part, ""),
        };
        if !key.is_empty() {
            if !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(format!("`{path}`: `{key}` is not a field's name"));
            }
            steps.push(Step::Key(key.to_string()));
        }
        let mut rest = rest;
        while let Some(r) = rest.strip_prefix("[*]") {
            steps.push(Step::Each);
            rest = r;
        }
        if !rest.is_empty() || (key.is_empty() && part.is_empty()) {
            return Err(format!(
                "`{path}`: a path is names joined by `.`, with `[*]` for an array's items"
            ));
        }
    }
    Ok(steps)
}

impl Filter {
    /// What is wrong with it, if anything.
    pub fn check(&self) -> Result<(), String> {
        if self.upstream.trim().is_empty() {
            return Err("a filter names its upstream".into());
        }
        if self.tool.trim().is_empty() {
            return Err("a filter names its tool".into());
        }
        if self.paths.is_empty() || self.paths.len() > PATHS_MAX {
            return Err(format!(
                "a filter names 1 to {PATHS_MAX} paths of the fields it filters"
            ));
        }
        for p in &self.paths {
            parse(p)?;
        }
        if self.command.first().is_none_or(|p| p.trim().is_empty()) {
            return Err("a filter's command names no program".into());
        }
        if let Some(w) = &self.write {
            if w.tool.trim().is_empty() {
                return Err("a write-back names its tool".into());
            }
            if self.paths.len() != 1 || self.paths[0].contains("[*]") {
                return Err(
                    "a write-back is for a filter with one path, without `[*]`: it stores one field"
                        .into(),
                );
            }
            if !w.arguments.values().any(|v| v == "$write") {
                return Err("a write-back passes `$write` in one of its arguments".into());
            }
            if let Some(v) = w
                .arguments
                .values()
                .find(|v| v.starts_with('$') && *v != "$write" && !v.starts_with("$args."))
            {
                return Err(format!(
                    "`{v}`: an argument is a text, `$write` or `$args.<name>`"
                ));
            }
        }
        Ok(())
    }
}

/// What is wrong with a list of filters, if anything.
pub fn check_all(filters: &[Filter]) -> Result<(), String> {
    if filters.len() > FILTERS_MAX {
        return Err(format!("at most {FILTERS_MAX} filters"));
    }
    for f in filters {
        f.check()
            .map_err(|e| format!("filter on `{}` `{}`: {e}", f.upstream, f.tool))?;
    }
    Ok(())
}

/// A command's answer for one field.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Answer {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<String>,
}

/// Why a command gave no answer. `log` is for the hub's log alone (it may
/// quote the command's stderr); `said` is what the agent is told.
#[derive(Debug)]
pub struct Failed {
    pub said: String,
    pub log: String,
}

/// Run `command` with `input` on stdin: its answer, or why none. Exit 0 and
/// 1 count (veilq ends with 1 when it replaced something); any other end, a
/// timeout, or output that is not `{text, write?}` does not.
pub fn run(command: &[String], input: &str, timeout: Duration) -> Result<Answer, Failed> {
    let name = command.first().map(String::as_str).unwrap_or_default();
    let fail = |said: String, log: String| Failed { said, log };
    let mut cmd = Command::new(crate::stdio::resolve(name));
    cmd.args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut call = devstack_proc::Call::spawn(&mut cmd).map_err(|e| {
        fail(
            format!("the filter `{name}` did not start: {e}"),
            format!("`{name}` did not start: {e}"),
        )
    })?;
    let child = call.child();
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let input = input.to_string();
    let feed = std::thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(input.as_bytes());
        }
        drop(stdin);
    });
    let drain = |p: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(p) = p {
                let _ = p.take(OUTPUT_MAX as u64).read_to_end(&mut out);
            }
            out
        })
    };
    let out = drain(stdout.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>));
    let err = drain(stderr.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>));
    let started = Instant::now();
    let status = loop {
        match call.child().try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() >= timeout => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    let Some(status) = status else {
        call.kill();
        let _ = feed.join();
        return Err(fail(
            format!(
                "the filter `{name}` took longer than {} s",
                timeout.as_secs()
            ),
            format!("`{name}` killed after {} s", timeout.as_secs()),
        ));
    };
    let _ = feed.join();
    let out = out.join().unwrap_or_default();
    let err = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
    let err: String = err.trim().chars().take(2000).collect();
    let code = status.code();
    if !matches!(code, Some(0 | 1)) {
        let code = code.map_or("a signal".to_string(), |c| c.to_string());
        return Err(fail(
            format!("the filter `{name}` failed (exit {code}); the hub's log has its output"),
            format!("`{name}` ended with {code}: {err}"),
        ));
    }
    serde_json::from_slice::<Answer>(&out).map_err(|e| {
        fail(
            format!("the filter `{name}` did not answer with JSON `{{text, write?}}`"),
            format!("`{name}` answered no `{{text, write?}}`: {e}"),
        )
    })
}

/// A write-back to make: the tool and its arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Store {
    pub tool: String,
    pub arguments: Value,
}

/// Runs a command on a field: [`run`], or a stand-in in a test.
pub type Runner<'a> = dyn FnMut(&[String], &str) -> Result<Answer, Failed> + 'a;

/// What filtering a result gave: the result for the agent, and what to
/// store back.
#[derive(Debug)]
pub struct Filtered {
    pub result: Value,
    pub stores: Vec<Store>,
}

/// Run `filters` (those of this upstream and tool) on `result`, a
/// `CallToolResult`, called with `arguments`; `answer` runs a command on a
/// field. An error result is handed on as it is: an upstream's error says
/// why it failed, not what a ticket holds.
pub fn apply(
    filters: &[&Filter],
    result: Value,
    arguments: &Value,
    answer: &mut Runner,
) -> Result<Filtered, Failed> {
    let said = |s: &str| Failed {
        said: s.to_string(),
        log: s.to_string(),
    };
    if filters.is_empty() || result["isError"] == json!(true) {
        return Ok(Filtered {
            result,
            stores: Vec::new(),
        });
    }
    let mut result = result;
    // The same text in the content and in `structuredContent` runs once.
    let mut memo: HashMap<(usize, String), Answer> = HashMap::new();
    let mut writes: Vec<(usize, String)> = Vec::new();
    let mut each = |doc: &mut Value| -> Result<(), Failed> {
        for (i, f) in filters.iter().enumerate() {
            for p in &f.paths {
                let steps = parse(p).map_err(|e| said(&e))?;
                let mut fields = Vec::new();
                find(doc, &steps, &mut fields).map_err(|e| said(&format!("`{p}`: {e}")))?;
                for v in fields {
                    let Value::String(s) = v else {
                        continue;
                    };
                    let a = match memo.get(&(i, s.clone())) {
                        Some(a) => a.clone(),
                        None => {
                            let a = answer(&f.command, s)?;
                            memo.insert((i, s.clone()), a.clone());
                            a
                        }
                    };
                    if let Some(w) = &a.write
                        && !writes.iter().any(|(j, x)| *j == i && x == w)
                    {
                        writes.push((i, w.clone()));
                    }
                    *v = Value::String(a.text);
                }
            }
        }
        Ok(())
    };
    let Some(content) = result.get_mut("content").and_then(Value::as_array_mut) else {
        return Err(said("the result has no content to filter"));
    };
    for item in content.iter_mut() {
        if item["type"] != "text" {
            return Err(said(
                "the result holds more than text, which a filter cannot read",
            ));
        }
        let Some(text) = item["text"].as_str() else {
            return Err(said("the result's text is missing"));
        };
        let mut doc: Value = serde_json::from_str(text)
            .map_err(|_| said("the result is not JSON, so its fields cannot be filtered"))?;
        each(&mut doc)?;
        item["text"] = Value::String(doc.to_string());
    }
    if let Some(s) = result.get_mut("structuredContent") {
        each(s)?;
    }
    let stores = writes
        .into_iter()
        .filter_map(|(i, w)| {
            let wb = filters[i].write.as_ref()?;
            let mut args = serde_json::Map::new();
            for (k, v) in &wb.arguments {
                let value = if v == "$write" {
                    Value::String(w.clone())
                } else if let Some(a) = v.strip_prefix("$args.") {
                    match arguments.get(a) {
                        Some(x) if !x.is_null() => x.clone(),
                        // Without it the write-back cannot name what it is for.
                        _ => return None,
                    }
                } else {
                    Value::String(v.clone())
                };
                args.insert(k.clone(), value);
            }
            Some(Store {
                tool: wb.tool.clone(),
                arguments: Value::Object(args),
            })
        })
        .collect();
    Ok(Filtered { result, stores })
}

/// Every value at `steps` below `v`, for writing: an absent key is nothing
/// to filter (the agent did not ask for the field); a step that meets the
/// wrong kind of value is an error, as the field might be there in a form
/// the filter does not reach.
fn find<'a>(v: &'a mut Value, steps: &[Step], out: &mut Vec<&'a mut Value>) -> Result<(), String> {
    let Some((step, rest)) = steps.split_first() else {
        match v {
            Value::String(_) | Value::Null => out.push(v),
            _ => return Err("the field is not a text".into()),
        }
        return Ok(());
    };
    match (step, v) {
        (_, Value::Null) => Ok(()),
        (Step::Key(k), Value::Object(m)) => match m.get_mut(k) {
            Some(x) => find(x, rest, out),
            None => Ok(()),
        },
        (Step::Each, Value::Array(a)) => {
            for x in a.iter_mut() {
                find(x, rest, out)?;
            }
            Ok(())
        }
        (Step::Key(k), _) => Err(format!("`{k}` is looked up in something not an object")),
        (Step::Each, _) => Err("`[*]` meets something not an array".into()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn filter(paths: &[&str]) -> Filter {
        Filter {
            upstream: "linear".into(),
            tool: "get_issue".into(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            command: vec!["veilq".into(), "mark".into(), "-".into()],
            write: None,
        }
    }

    fn text_result(doc: Value) -> Value {
        json!({ "content": [{ "type": "text", "text": doc.to_string() }] })
    }

    fn mark(_: &[String], s: &str) -> Result<Answer, Failed> {
        Ok(Answer {
            text: format!("clean:{}", s.len()),
            write: Some(format!("marked:{s}")),
        })
    }

    #[test]
    fn paths_parse_and_refuse() {
        assert_eq!(
            parse("issues[*].description").unwrap(),
            vec![
                Step::Key("issues".into()),
                Step::Each,
                Step::Key("description".into())
            ]
        );
        assert_eq!(parse("[*].body").unwrap().len(), 2);
        for bad in ["", "a..b", "a[0]", "a[*]x", "a b", "."] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_write_back_needs_one_plain_path_and_write() {
        let mut f = filter(&["description"]);
        f.write = Some(WriteBack {
            tool: "save_issue".into(),
            arguments: [
                ("id".to_string(), "$args.id".to_string()),
                ("description".to_string(), "$write".to_string()),
            ]
            .into(),
        });
        assert!(f.check().is_ok());
        let mut g = f.clone();
        g.paths = vec!["issues[*].description".into()];
        assert!(g.check().is_err());
        let mut g = f.clone();
        g.write.as_mut().unwrap().arguments.remove("description");
        assert!(g.check().is_err());
        let mut g = f.clone();
        g.write
            .as_mut()
            .unwrap()
            .arguments
            .insert("x".into(), "$env.HOME".into());
        assert!(g.check().is_err());
        assert!(filter(&[]).check().is_err());
    }

    #[test]
    fn the_field_is_replaced_in_text_and_structured_content_once() {
        let mut f = filter(&["description"]);
        f.write = Some(WriteBack {
            tool: "save_issue".into(),
            arguments: [
                ("id".to_string(), "$args.id".to_string()),
                ("description".to_string(), "$write".to_string()),
            ]
            .into(),
        });
        let doc = json!({ "id": "X-1", "title": "t", "description": "call Lena" });
        let mut r = text_result(doc.clone());
        r["structuredContent"] = doc;
        let mut runs = 0;
        let out = apply(&[&f], r, &json!({ "id": "X-1" }), &mut |c, s| {
            runs += 1;
            mark(c, s)
        })
        .unwrap();
        assert_eq!(runs, 1);
        let text: Value =
            serde_json::from_str(out.result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["description"], "clean:9");
        assert_eq!(text["title"], "t");
        assert_eq!(out.result["structuredContent"]["description"], "clean:9");
        assert_eq!(
            out.stores,
            vec![Store {
                tool: "save_issue".into(),
                arguments: json!({ "id": "X-1", "description": "marked:call Lena" }),
            }]
        );
    }

    #[test]
    fn arrays_are_walked_and_absent_fields_are_nothing() {
        let f = filter(&["issues[*].description"]);
        let r = text_result(json!({ "issues": [
            { "id": "a", "description": "one" },
            { "id": "b" },
            { "id": "c", "description": null },
        ]}));
        let out = apply(&[&f], r, &json!({}), &mut mark).unwrap();
        let text: Value =
            serde_json::from_str(out.result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["issues"][0]["description"], "clean:3");
        assert!(text["issues"][1].get("description").is_none());
        assert!(text["issues"][2]["description"].is_null());
        assert!(out.stores.is_empty(), "no write-back configured");
        // The agent left the field out.
        let r = text_result(json!({ "id": "X-1", "title": "t" }));
        assert!(apply(&[&filter(&["description"])], r, &json!({}), &mut mark).is_ok());
    }

    #[test]
    fn it_fails_closed() {
        let f = filter(&["description"]);
        let doc = json!({ "description": "call Lena" });
        // The command fails.
        let e = apply(&[&f], text_result(doc.clone()), &json!({}), &mut |_, _| {
            Err(Failed {
                said: "no".into(),
                log: "no".into(),
            })
        })
        .unwrap_err();
        assert_eq!(e.said, "no");
        // Not JSON.
        let r = json!({ "content": [{ "type": "text", "text": "call Lena" }] });
        assert!(apply(&[&f], r, &json!({}), &mut mark).is_err());
        // More than text.
        let r = json!({ "content": [{ "type": "image", "data": "", "mimeType": "image/png" }] });
        assert!(apply(&[&f], r, &json!({}), &mut mark).is_err());
        // The field in a form the filter does not reach.
        let r = text_result(json!({ "description": { "md": "call Lena" } }));
        assert!(apply(&[&f], r, &json!({}), &mut mark).is_err());
        // An upstream's error is handed on.
        let r = json!({ "content": [{ "type": "text", "text": "not found" }], "isError": true });
        assert!(apply(&[&f], r, &json!({}), &mut mark).is_ok());
    }

    /// A program that reads stdin and prints `script`'s output, on every
    /// platform: the test binary's own shell.
    pub(crate) fn shell(script: &str) -> Vec<String> {
        if cfg!(windows) {
            vec![
                "powershell".into(),
                "-NoProfile".into(),
                "-Command".into(),
                script.into(),
            ]
        } else {
            vec!["sh".into(), "-c".into(), script.into()]
        }
    }

    #[test]
    fn a_command_answers_or_fails() {
        let ok = if cfg!(windows) {
            shell(
                "$t = [Console]::In.ReadToEnd(); ConvertTo-Json -Compress @{text=[string]$t.Length}; exit 1",
            )
        } else {
            shell("t=$(cat); printf '{\"text\":\"%s\"}' \"${#t}\"; exit 1")
        };
        let a = run(&ok, "abcd", TIMEOUT).unwrap();
        assert_eq!(a.text, "4");
        let bad = shell("exit 2");
        assert!(run(&bad, "x", TIMEOUT).unwrap_err().said.contains("exit 2"));
        let garbage = shell("echo hello");
        assert!(run(&garbage, "x", TIMEOUT).is_err());
        assert!(run(&["devstack-no-such-filter".into()], "x", TIMEOUT).is_err());
        let slow = if cfg!(windows) {
            shell("Start-Sleep -Seconds 20")
        } else {
            shell("sleep 20")
        };
        let t = Instant::now();
        assert!(
            run(&slow, "x", Duration::from_millis(500))
                .unwrap_err()
                .said
                .contains("longer")
        );
        assert!(t.elapsed() < Duration::from_secs(10));
    }
}
