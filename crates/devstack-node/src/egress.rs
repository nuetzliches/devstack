//! What a lane's networks may reach (ADR-0033 point 5), for the node's
//! network mode `internet`: the internet, but no private address range (the
//! node's local network, the host itself). Enforced on the Docker host by
//! iptables rules per lane: a chain that `DOCKER-USER` jumps to for the
//! lane's forwarded traffic, one that `INPUT` jumps to for traffic to the
//! host. They run in a small helper container on the host's network, so a
//! node agent in a container (or on a machine whose Docker runs in a VM)
//! sets them where Docker forwards.
//!
//! For `none`, the same rules hold, and nothing else leaves the lane's
//! bridges either (its networks are `internal` too).
//!
//! Rules are added once, after every network of the lane exists and before
//! any of its containers start, and removed when the lane stops; adding
//! removes what an earlier start of the same lease left. A lane's networks
//! have no IPv6 (the node overlay and `--ipv6=false`), so IPv4 rules hold.

use crate::policy::Network;
use std::io::Write;
use std::path::Path;
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

/// The shell script that sets a lease's rules for its networks: each IPv4
/// subnet with the bridge it is on. Traffic to the lane's own subnets
/// returns only when it leaves through the lane's own bridge, so a subnet a
/// Compose file chose cannot open a range of the node's network.
pub fn apply_script(lease: &str, nets: &[(String, String)], mode: Network) -> String {
    let (f, i) = (chain(lease, 'f'), chain(lease, 'i'));
    let mut s = String::from(
        "set -e
",
    );
    s.push_str(&remove_script(lease));
    // Forwarded: answers, the lane's own networks, then no private range.
    s.push_str(&format!(
        "iptables -N {f}
"
    ));
    s.push_str(&format!(
        "iptables -A {f} -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
"
    ));
    for (n, bridge) in nets {
        s.push_str(&format!(
            "iptables -A {f} -d {n} -o {bridge} -j RETURN
"
        ));
    }
    if mode == Network::None {
        s.push_str(&format!(
            "iptables -A {f} -j DROP
"
        ));
    } else {
        for b in BLOCKED {
            s.push_str(&format!(
                "iptables -A {f} -d {b} -j DROP
"
            ));
        }
    }
    // To the host: answers only, not even through the lane's gateway.
    s.push_str(&format!(
        "iptables -N {i}
"
    ));
    s.push_str(&format!(
        "iptables -A {i} -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
"
    ));
    s.push_str(&format!(
        "iptables -A {i} -j DROP
"
    ));
    for (n, _) in nets {
        s.push_str(&format!(
            "iptables -I DOCKER-USER -s {n} -j {f}
"
        ));
        s.push_str(&format!(
            "iptables -I INPUT -s {n} -j {i}
"
        ));
    }
    s
}

fn helper(script: &str) -> Result<(), String> {
    let mut cmd = Command::new("docker");
    cmd.args([
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
    ]);
    let out = devstack_compose::output_within(cmd, devstack_compose::budget::QUICK)
        .map_err(|e| format!("docker: {e}"))?
        .ok_or("docker: the rules' helper did not finish")?;
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

/// Whether this node ever set rules (the helper image may be gone since).
fn marker(state: &Path) -> std::path::PathBuf {
    state.join("egress-rules")
}

/// Keep the lane's networks (`nets`: subnet, bridge) from private address
/// ranges (`internet`), or from everything outside the lane (`none`).
pub fn apply(
    state: &Path,
    lease: &str,
    nets: &[(String, String)],
    mode: Network,
) -> Result<(), String> {
    let _ = std::fs::write(marker(state), b"");
    ensure_image()?;
    helper(&apply_script(lease, nets, mode))
        .map_err(|e| format!("network rules for the lane ({mode:?}): {e}"))
}

/// Remove the rules of lanes this node no longer holds (a lane that stopped
/// while the agent was down left them; a later network with the same subnet
/// would be held to them). Only on a node that ever set rules.
pub fn cleanup(state: &Path, keep: &[String]) {
    if !marker(state).exists() || ensure_image().is_err() {
        return;
    }
    let _ = helper(&cleanup_script(keep));
}

/// The shell script that removes every lane's rules but those of `keep`.
pub fn cleanup_script(keep: &[String]) -> String {
    let keep: Vec<String> = keep.iter().map(|l| chain(l, 'f')).collect();
    // Every forward chain of ours that is not kept, with its input chain.
    let keep = keep.join(" ");
    let script = format!(
        "for c in $(iptables -S | sed -n 's/^-N \\(dsl-[A-Za-z0-9]*\\)-f$/\\1/p'); do\n\
         case \" {keep} \" in *\" $c-f \"*) continue;; esac\n\
         for p in DOCKER-USER INPUT; do iptables -S $p | grep -E -- \"-j $c-(f|i)$\" | sed 's/^-A/-D/' | while read -r r; do iptables $r; done; done\n\
         for k in f i; do iptables -F $c-$k 2>/dev/null; iptables -X $c-$k 2>/dev/null; done\n\
         done\n"
    );
    script
}

/// Remove the lane's rules; nothing on a node that never set any.
pub fn remove(state: &Path, lease: &str) {
    if marker(state).exists() && ensure_image().is_ok() {
        let _ = helper(&remove_script(lease));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_let_the_lane_reach_itself_and_the_internet_only() {
        let nets = [("172.30.0.0/16".to_string(), "br-0123456789ab".to_string())];
        let s = apply_script("Ab-12cd34ef56gh78", &nets, Network::Internet);
        let f = "dsl-Ab12cd34ef56-f";
        assert!(s.contains(&format!(
            "iptables -A {f} -d 172.30.0.0/16 -o br-0123456789ab -j RETURN"
        )));
        assert!(s.contains(&format!("iptables -A {f} -d 192.168.0.0/16 -j DROP")));
        assert!(!s.contains(&format!("iptables -A {f} -j DROP")));
        assert!(s.contains(&format!("iptables -I DOCKER-USER -s 172.30.0.0/16 -j {f}")));
        assert!(s.contains("iptables -I INPUT -s 172.30.0.0/16 -j dsl-Ab12cd34ef56-i"));
        // The lane's own range comes before the private ranges it lies in.
        assert!(s.find("-d 172.30.0.0/16 -o") < s.find("-d 172.16.0.0/12 -j DROP"));
        // Starting again removes the earlier rules first.
        assert!(s.find(&format!("iptables -X {f}")) < s.find(&format!("iptables -N {f}")));
        // `none`: nothing but the lane's own bridges.
        let s = apply_script("Ab-12cd34ef56gh78", &nets, Network::None);
        assert!(s.contains(&format!("iptables -A {f} -j DROP")));
        assert!(!s.contains("192.168.0.0/16"));
    }
}
