//! Telemetry to the pool's collector (ADR-0037): configured on the
//! coordinator only (an OTLP/HTTP endpoint and its bearer token; none: off),
//! sent by members over their mTLS connection to the coordinator, which sets
//! the sender's identity from its certificate and passes it on. A member
//! sends only with its consent, kept per pool beside its membership
//! (`telemetry.json`, off until given); a node sends what concerns the lanes
//! it runs for the pool with the coordinator's setting alone.
//!
//! Never sent: file contents, environment values, command lines, service
//! output. Lossy by design: what cannot be sent is dropped.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

/// The collector a coordinator passes telemetry to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Collector {
    /// OTLP/HTTP base, e.g. `https://collector.example/tenant`; `/v1/logs`
    /// and `/v1/metrics` are added.
    #[serde(default)]
    pub endpoint: String,
    /// Sent as `Authorization: Bearer <token>`.
    #[serde(default)]
    pub token: String,
}

impl Collector {
    /// `DEVSTACK_TELEMETRY_ENDPOINT` and `DEVSTACK_TELEMETRY_TOKEN`, else
    /// `telemetry.json` in the coordinator's directory; `None`: off.
    pub fn of(dir: &Path) -> Option<Self> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let c = match env("DEVSTACK_TELEMETRY_ENDPOINT") {
            Some(endpoint) => Self {
                endpoint,
                token: env("DEVSTACK_TELEMETRY_TOKEN").unwrap_or_default(),
            },
            None => std::fs::read(dir.join("telemetry.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())?,
        };
        (!c.endpoint.trim().is_empty()).then_some(c)
    }

    /// Pass an OTLP/JSON body on; `kind` is `logs` or `metrics`. Blocking.
    pub fn send(&self, kind: &str, body: &Value) -> Result<(), String> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let url = format!("{}/v1/{kind}", self.endpoint.trim_end_matches('/'));
        let mut req = agent.post(&url).header("Content-Type", "application/json");
        if !self.token.is_empty() {
            req = req.header("Authorization", &format!("Bearer {}", self.token));
        }
        let res = req.send(body.to_string()).map_err(|e| e.to_string())?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(format!("the collector answered {}", res.status()))
        }
    }
}

/// What a member agreed to send its pool (ADR-0037 point 3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Consent {
    #[serde(default)]
    pub enabled: bool,
}

impl Consent {
    pub fn load(dir: &Path) -> Self {
        std::fs::read(dir.join("telemetry.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(
            dir.join("telemetry.json"),
            serde_json::to_vec_pretty(self).expect("consent serialises"),
        )
    }
}

/// `GET /v1/telemetry`: whether the pool collects, and what.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Info {
    pub enabled: bool,
    /// What members send, in words, for the dashboard.
    #[serde(default)]
    pub collects: Vec<String>,
}

/// What is sent, as the pool area says it.
pub fn collects() -> Vec<String> {
    [
        "devstack's diagnostic log (its decisions with their reasons)",
        "durations and outcomes of lane starts, syncs, moves and builds",
        "how many lanes and services run, and a node's capacity and use",
        "labels: repository, lane, service, node, member, devstack version, OS",
        "never: file contents, environment values, command lines, service output",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Set the sender's identity on every resource of an OTLP/JSON body,
/// overwriting what the body claims (`devstack.member.*`, `devstack.pool.*`).
pub fn stamp(body: &mut Value, ours: &[(&str, String)]) {
    for list in ["resourceLogs", "resourceMetrics", "resourceSpans"] {
        let Some(rs) = body.get_mut(list).and_then(Value::as_array_mut) else {
            continue;
        };
        for r in rs {
            let attrs = r["resource"]["attributes"].take();
            let mut attrs: Vec<Value> = attrs
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|a| {
                    let k = a["key"].as_str().unwrap_or("");
                    !k.starts_with("devstack.member.") && !k.starts_with("devstack.pool.")
                })
                .collect();
            for (k, v) in ours {
                attrs.push(json!({ "key": k, "value": { "stringValue": v } }));
            }
            r["resource"]["attributes"] = Value::Array(attrs);
        }
    }
}

/// The resource of what a process sends: its service name, devstack's
/// version, the operating system.
pub fn resource(service: &str) -> Value {
    json!({ "attributes": [
        { "key": "service.name", "value": { "stringValue": service } },
        { "key": "service.version", "value": { "stringValue": env!("CARGO_PKG_VERSION") } },
        { "key": "os.type", "value": { "stringValue": std::env::consts::OS } },
    ]})
}

fn attrs(labels: &[(String, String)]) -> Vec<Value> {
    labels
        .iter()
        .map(|(k, v)| json!({ "key": k, "value": { "stringValue": v } }))
        .collect()
}

/// A gauge's value now, with its labels.
pub struct Gauge {
    pub name: String,
    pub unit: String,
    pub value: f64,
    pub labels: Vec<(String, String)>,
}

/// Durations of one kind (ms), with their labels: one histogram point.
pub struct Durations {
    pub name: String,
    pub values: Vec<f64>,
    pub labels: Vec<(String, String)>,
}

/// Bucket bounds of the duration histograms, ms.
const BOUNDS: [f64; 10] = [
    250.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 30000.0, 60000.0, 120000.0, 300000.0,
];

/// OTLP/JSON `ExportMetricsServiceRequest`.
pub fn metrics_body(service: &str, gauges: &[Gauge], durations: &[Durations]) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
        .to_string();
    let mut metrics: Vec<Value> = gauges
        .iter()
        .map(|g| {
            json!({
                "name": g.name, "unit": g.unit,
                "gauge": { "dataPoints": [{
                    "timeUnixNano": now, "asDouble": g.value, "attributes": attrs(&g.labels),
                }]},
            })
        })
        .collect();
    for d in durations.iter().filter(|d| !d.values.is_empty()) {
        let mut counts = vec![0u64; BOUNDS.len() + 1];
        for v in &d.values {
            let i = BOUNDS.iter().position(|b| v <= b).unwrap_or(BOUNDS.len());
            counts[i] += 1;
        }
        let min = d.values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = d.values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        metrics.push(json!({
            "name": d.name, "unit": "ms",
            "histogram": {
                "aggregationTemporality": 1,
                "dataPoints": [{
                    "timeUnixNano": now,
                    "count": d.values.len().to_string(),
                    "sum": d.values.iter().sum::<f64>(),
                    "min": min, "max": max,
                    "bucketCounts": counts.iter().map(u64::to_string).collect::<Vec<_>>(),
                    "explicitBounds": BOUNDS,
                    "attributes": attrs(&d.labels),
                }],
            },
        }));
    }
    json!({ "resourceMetrics": [{
        "resource": resource(service),
        "scopeMetrics": [{ "scope": { "name": "devstack" }, "metrics": metrics }],
    }]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_coordinator_sets_who_sent_it() {
        let mut b = json!({ "resourceLogs": [{ "resource": { "attributes": [
            { "key": "service.name", "value": { "stringValue": "devstack-hub" } },
            { "key": "devstack.member.id", "value": { "stringValue": "someone-else" } },
        ]}}]});
        stamp(&mut b, &[("devstack.member.id", "abc".into())]);
        let a = b["resourceLogs"][0]["resource"]["attributes"]
            .as_array()
            .unwrap();
        let ids: Vec<&str> = a
            .iter()
            .filter(|x| x["key"] == "devstack.member.id")
            .map(|x| x["value"]["stringValue"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["abc"]);
        assert!(a.iter().any(|x| x["key"] == "service.name"));
    }

    #[test]
    fn durations_become_a_histogram() {
        let b = metrics_body(
            "devstack-daemon",
            &[],
            &[Durations {
                name: "devstack.lane.start".into(),
                values: vec![100.0, 1500.0, 400000.0],
                labels: vec![],
            }],
        );
        let h =
            &b["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0]["histogram"]["dataPoints"][0];
        assert_eq!(h["count"], "3");
        let counts: Vec<&str> = h["bucketCounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert_eq!(counts[0], "1"); // <= 250
        assert_eq!(counts[3], "1"); // <= 2000
        assert_eq!(counts[10], "1"); // above the last bound
    }
}
