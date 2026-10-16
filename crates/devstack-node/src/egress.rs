//! What a lane's networks may reach (ADR-0033 point 5), for the node's
//! network mode `internet`: the internet, but no private address range (the
//! node's local network, the host itself). Enforced on the Docker host by
//! iptables rules per lane: a chain that `DOCKER-USER` jumps to for the
//! lane's forwarded traffic, one that `INPUT` jumps to for traffic to the
//! host. They run in a small helper container on the host's network, so a
//! node agent in a container (or on a machine whose Docker runs in a VM)
//! sets them where Docker forwards.
//!
//! Rules are added before the lane's containers start and removed when the
//! lane stops; adding removes what an earlier start of the same lease left.

use std::io::Write;
use std::process::{Command, Stdio};

/// The helper image, built on the node once.
const IMAGE: &str = "devstack-egress:1";
const DOCKERFILE: &str = "FROM alpine:3\nRUN apk add --no-cache iptables\n";

/// What `internet` keeps a lane from: private, shared, link-local and
/// multicast addresses.
const BLOCKED: [&str; 6] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "169.254.0.0/16",
    "224.0.0.0/4",
];

/// The chains of a lease: `dsl-<id>-f` (forward), `dsl-<id>-i` (input).
fn chain(lease: &str, kind: char) -> String {
    let id: String = lease
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    format!("dsl-{id}-{kind}")
}

/// The shell script that removes a lease's rules (nothing if there are none).
fn remove_script(lease: &str) -> String {
    let mut s = String::new();
    for (parent, kind) in [("DOCKER-USER", 'f'), ("INPUT", 'i')] {
        let c = chain(lease, kind);
        s.push_str(&format!(
            "iptables -S {parent} | grep -- '-j {c}$' | sed 's/^-A/-D/' | while read -r r; do iptables $r; done\n\
             iptables -F {c} 2>/dev/null || true; iptables -X {c} 2>/dev/null || true\n"
        ));
    }
    s
}

/// The shell script that sets a lease's rules for its networks' `subnets`.
pub fn apply_script(lease: &str, subnets: &[String]) -> String {
    let (f, i) = (chain(lease, 'f'), chain(lease, 'i'));
    let mut s = String::from("set -e\n");
    s.push_str(&remove_script(lease));
    // Forwarded: answers, the lane's own networks, then no private range.
    s.push_str(&format!("iptables -N {f}\n"));
    s.push_str(&format!(
        "iptables -A {f} -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN\n"
    ));
    for n in subnets {
        s.push_str(&format!("iptables -A {f} -d {n} -j RETURN\n"));
    }
    for b in BLOCKED {
        s.push_str(&format!("iptables -A {f} -d {b} -j DROP\n"));
    }
    // To the host: answers only, not even through the lane's gateway.
    s.push_str(&format!("iptables -N {i}\n"));
    s.push_str(&format!(
        "iptables -A {i} -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN\n"
    ));
    s.push_str(&format!("iptables -A {i} -j DROP\n"));
    for n in subnets {
        s.push_str(&format!("iptables -I DOCKER-USER -s {n} -j {f}\n"));
        s.push_str(&format!("iptables -I INPUT -s {n} -j {i}\n"));
    }
    s
}

fn helper(script: &str) -> Result<(), String> {
    let out = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--net=host",
            "--cap-add",
            "NET_ADMIN",
            "--cap-add",
            "NET_RAW",
            IMAGE,
            "sh",
            "-c",
            script,
        ])
        .output()
        .map_err(|e| format!("docker: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn ensure_image() -> Result<(), String> {
    let have = Command::new("docker")
        .args(["image", "inspect", IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if have {
        return Ok(());
    }
    let mut child = Command::new("docker")
        .args(["build", "-q", "-t", IMAGE, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("docker build: {e}"))?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(DOCKERFILE.as_bytes())
        .map_err(|e| format!("docker build: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("docker build: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "building {IMAGE}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Keep the lane's networks (`subnets`) from private address ranges.
pub fn apply(lease: &str, subnets: &[String]) -> Result<(), String> {
    ensure_image()?;
    helper(&apply_script(lease, subnets))
        .map_err(|e| format!("network rules for the lane (internet only): {e}"))
}

/// Remove the lane's rules; nothing if it has none.
pub fn remove(lease: &str) {
    if ensure_image().is_ok() {
        let _ = helper(&remove_script(lease));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rules_let_the_lane_reach_itself_and_the_internet_only() {
        let s = super::apply_script("Ab-12cd34ef56gh78", &["172.30.0.0/16".into()]);
        let f = "dsl-Ab12cd34ef56-f";
        assert!(s.contains(&format!("iptables -A {f} -d 172.30.0.0/16 -j RETURN")));
        assert!(s.contains(&format!("iptables -A {f} -d 192.168.0.0/16 -j DROP")));
        assert!(s.contains(&format!("iptables -I DOCKER-USER -s 172.30.0.0/16 -j {f}")));
        assert!(s.contains("iptables -I INPUT -s 172.30.0.0/16 -j dsl-Ab12cd34ef56-i"));
        // The lane's own range comes before the private ranges it lies in.
        assert!(s.find("-d 172.30.0.0/16 -j RETURN") < s.find("-d 172.16.0.0/12 -j DROP"));
        // Starting again removes the earlier rules first.
        assert!(s.find(&format!("iptables -X {f}")) < s.find(&format!("iptables -N {f}")));
    }
}
