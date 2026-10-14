//! `devstack top`: what the stack costs the machine (plan §12).

use crate::client;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub fn run(stack_file: &Path, watch: bool) -> Result<()> {
    let c = client::need(&client::state_dir(stack_file)?)?;
    loop {
        let report: Value = serde_json::from_str(&c.get("/v1/usage")?)?;
        if watch {
            print!("\x1b[2J\x1b[H");
        }
        print!("{}", render(&report));
        if !watch {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub fn render(r: &Value) -> String {
    let rows = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    let mut line = |cols: [String; 4]| {
        let l = format!(
            "{:<28} {:>7} {:>11} {:>6}",
            cols[0], cols[1], cols[2], cols[3]
        );
        out.push_str(l.trim_end());
        out.push('\n');
    };
    line([
        "SERVICE".into(),
        "CPU".into(),
        "MEMORY".into(),
        "PROCS".into(),
    ]);
    for s in rows(&r["services"]).iter().chain([&r["builds"]]) {
        let running = s.get("processes").is_some();
        line([
            s["name"].as_str().unwrap_or("").into(),
            if running { cpu(s) } else { "-".into() },
            if running { mem(s) } else { "-".into() },
            s["processes"]
                .as_u64()
                .map_or("-".into(), |p| p.to_string()),
        ]);
    }
    let containers = rows(&r["containers"]);
    if !containers.is_empty() {
        line([String::new(), String::new(), String::new(), String::new()]);
        line([
            "CONTAINER".into(),
            "CPU".into(),
            "MEMORY".into(),
            String::new(),
        ]);
        for c in &containers {
            line([
                format!(
                    "{}/{}",
                    c["group"].as_str().unwrap_or(""),
                    c["service"].as_str().unwrap_or("")
                ),
                cpu(c),
                mem(c),
                String::new(),
            ]);
        }
    }
    let groups = rows(&r["groups"]);
    if !groups.is_empty() {
        line([String::new(), String::new(), String::new(), String::new()]);
        line(["GROUP".into(), "CPU".into(), "MEMORY".into(), String::new()]);
        for g in &groups {
            line([
                g["name"].as_str().unwrap_or("").into(),
                cpu(g),
                mem(g),
                String::new(),
            ]);
        }
    }
    for l in rows(&r["lanes"]) {
        out.push_str(&format!(
            "\nlane {}: {} CPU, {} memory",
            l["name"].as_str().unwrap_or(""),
            cpu(&l),
            mem(&l)
        ));
        let d = &l["disk"];
        if d.is_object() {
            out.push_str(&format!(
                "; disk: snapshot {}, artifacts {}, logs {}, volumes {}",
                bytes(d["snapshot"].as_u64()),
                bytes(d["artifacts"].as_u64()),
                bytes(d["logs"].as_u64()),
                bytes(d["volumes"].as_u64())
            ));
        }
        out.push('\n');
    }
    if let Some(e) = r["docker_error"].as_str() {
        out.push_str(&format!(
            "containers not measured: {}\n",
            e.lines().next().unwrap_or("")
        ));
    }
    out
}

fn cpu(v: &Value) -> String {
    format!("{:.1}%", v["cpu"].as_f64().unwrap_or(0.0))
}

fn mem(v: &Value) -> String {
    bytes(v["memory"].as_u64())
}

fn bytes(n: Option<u64>) -> String {
    let Some(n) = n else {
        return "-".into();
    };
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_report() {
        let r = serde_json::json!({
            "services": [
                {"name": "api", "cpu": 12.34, "memory": 89_128_960u64, "processes": 2},
                {"name": "worker", "cpu": 0.0, "memory": 0}
            ],
            "builds": {"name": "builds", "cpu": 0.0, "memory": 0, "processes": 0},
            "containers": [{"group": "infra", "service": "db", "cpu": 0.4, "memory": 1024}],
            "groups": [{"name": "app", "cpu": 12.34, "memory": 89_128_960u64}],
            "lanes": [{"name": "main", "cpu": 12.74, "memory": 89_129_984u64,
                       "disk": {"snapshot": 2048, "artifacts": 0, "logs": 10, "volumes": null}}]
        });
        let text = render(&r);
        assert!(text.contains("api"), "{text}");
        assert!(
            text.contains("12.3%") && text.contains("85.0 MiB"),
            "{text}"
        );
        assert!(
            text.contains("infra/db") && text.contains("1.0 KiB"),
            "{text}"
        );
        assert!(
            text.contains("worker")
                && text
                    .lines()
                    .any(|l| l.starts_with("worker") && l.contains(" - ")),
            "{text}"
        );
        assert!(
            text.contains("snapshot 2.0 KiB") && text.contains("volumes -"),
            "{text}"
        );
    }
}
