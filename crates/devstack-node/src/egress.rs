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
//!
//! Several agents may share one Docker engine (one per pool, ADR-0034): the
//! chains carry the agent's owner tag (`crate::lanerun::owner_tag`), and an
//! agent removes only its own.

use crate::policy::Network;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// One change to the rules at a time in this agent: a sweep reads which
/// leases it holds and removes the other chains with nothing set between.
static RULES: Mutex<()> = Mutex::new(());

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

/// Up to `n` ASCII letters and digits of `v`: what goes into a chain name.
fn alnum(v: &str, n: usize) -> String {
    v.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(n)
        .collect()
}

/// The chains of a lease of the agent `owner`: `ds-<owner>-<id>-f`
/// (forward), `ds-<owner>-<id>-i` (input). At most 26 characters, under
/// iptables' 28.
fn chain(owner: &str, lease: &str, kind: char) -> String {
    format!("ds-{}-{}-{kind}", alnum(owner, 8), alnum(lease, 12))
}

/// The chains as an agent named them before they carried their owner:
/// `dsl-<id>-f`, `dsl-<id>-i`. Removed with their lease, never set.
fn legacy_chain(lease: &str, kind: char) -> String {
    format!("dsl-{}-{kind}", alnum(lease, 12))
}

/// The shell lines that remove the chain pair `<c>-f`/`<c>-i` (`$c` may be
/// a shell variable) and the jumps to it.
fn remove_pair(c: &str) -> String {
    format!(
        "for p in DOCKER-USER INPUT; do iptables -S $p | grep -E -- \"-j {c}-(f|i)$\" | sed 's/^-A/-D/' | while read -r r; do iptables $r; done; done\n\
         for k in f i; do iptables -F {c}-$k 2>/dev/null || true; iptables -X {c}-$k 2>/dev/null || true; done\n"
    )
}

/// The shell script that removes a lease's rules (nothing if there are
/// none), under the name an older agent gave them too.
fn remove_script(owner: &str, lease: &str) -> String {
    let mut s = String::new();
    for c in [chain(owner, lease, 'f'), legacy_chain(lease, 'f')] {
        s.push_str(&remove_pair(c.trim_end_matches("-f")));
    }
    s
}

/// The shell script that sets a lease's rules for its networks: each IPv4
/// subnet with the bridge it is on. Traffic to the lane's own subnets
/// returns only when it leaves through the lane's own bridge, so a subnet a
/// Compose file chose cannot open a range of the node's network.
pub fn apply_script(owner: &str, lease: &str, nets: &[(String, String)], mode: Network) -> String {
    let (f, i) = (chain(owner, lease, 'f'), chain(owner, lease, 'i'));
    let mut s = String::from(
        "set -e
",
    );
    s.push_str(&remove_script(owner, lease));
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
    owner: &str,
    lease: &str,
    nets: &[(String, String)],
    mode: Network,
) -> Result<(), String> {
    let _guard = RULES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = std::fs::write(marker(state), b"");
    ensure_image()?;
    helper(&apply_script(owner, lease, nets, mode))
        .map_err(|e| format!("network rules for the lane ({mode:?}): {e}"))
}

/// Remove the rules of `owner`'s lanes it no longer holds (a lane that
/// stopped while the agent was down left them; a later network with the
/// same subnet would be held to them). Only on a node that ever set rules.
///
/// `leases` says, when the rules are about to be changed (nothing is set
/// meanwhile), the leases this agent holds and the leases any lane
/// container on the engine has, whichever agent's: a chain an older agent
/// named without its owner goes only when no container has its lease.
pub fn cleanup(state: &Path, owner: &str, leases: impl FnOnce() -> (Vec<String>, Vec<String>)) {
    if !marker(state).exists() || ensure_image().is_err() {
        return;
    }
    let _guard = RULES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (held, live) = leases();
    let _ = helper(&cleanup_script(owner, &held, &live));
}

/// The shell script that removes `owner`'s lanes' rules but those of
/// `held`, and the rules an older agent left (named without an owner) of
/// leases neither in `held` nor in `live`. Another agent's rules stay.
pub fn cleanup_script(owner: &str, held: &[String], live: &[String]) -> String {
    let keep: Vec<String> = held.iter().map(|l| chain(owner, l, 'f')).collect();
    let keep_legacy: Vec<String> = held
        .iter()
        .chain(live)
        .map(|l| legacy_chain(l, 'f'))
        .collect();
    let (keep, keep_legacy) = (keep.join(" "), keep_legacy.join(" "));
    let owner = alnum(owner, 8);
    let remove = remove_pair("$c");
    // Every forward chain that is not kept, with its input chain.
    format!(
        "for c in $(iptables -S | sed -n 's/^-N \\(ds-{owner}-[A-Za-z0-9]*\\)-f$/\\1/p'); do\n\
         case \" {keep} \" in *\" $c-f \"*) continue;; esac\n\
         {remove}\
         done\n\
         for c in $(iptables -S | sed -n 's/^-N \\(dsl-[A-Za-z0-9]*\\)-f$/\\1/p'); do\n\
         case \" {keep_legacy} \" in *\" $c-f \"*) continue;; esac\n\
         {remove}\
         done\n"
    )
}

/// Remove the lane's rules; nothing on a node that never set any.
pub fn remove(state: &Path, owner: &str, lease: &str) {
    if marker(state).exists() && ensure_image().is_ok() {
        let _guard = RULES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = helper(&remove_script(owner, lease));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_let_the_lane_reach_itself_and_the_internet_only() {
        let nets = [("172.30.0.0/16".to_string(), "br-0123456789ab".to_string())];
        let s = apply_script("0a1b2c3d", "Ab-12cd34ef56gh78", &nets, Network::Internet);
        let f = "ds-0a1b2c3d-Ab12cd34ef56-f";
        assert!(f.len() <= 28, "iptables takes chain names of 28 characters");
        assert!(s.contains(&format!(
            "iptables -A {f} -d 172.30.0.0/16 -o br-0123456789ab -j RETURN"
        )));
        assert!(s.contains(&format!("iptables -A {f} -d 192.168.0.0/16 -j DROP")));
        assert!(!s.contains(&format!("iptables -A {f} -j DROP")));
        assert!(s.contains(&format!("iptables -I DOCKER-USER -s 172.30.0.0/16 -j {f}")));
        assert!(s.contains("iptables -I INPUT -s 172.30.0.0/16 -j ds-0a1b2c3d-Ab12cd34ef56-i"));
        // The lane's own range comes before the private ranges it lies in.
        assert!(s.find("-d 172.30.0.0/16 -o") < s.find("-d 172.16.0.0/12 -j DROP"));
        // Starting again removes the earlier rules first, an older agent's
        // name for them too.
        let base = "ds-0a1b2c3d-Ab12cd34ef56";
        assert!(s.find(&format!("iptables -X {base}-$k")) < s.find(&format!("iptables -N {f}")));
        assert!(s.contains("iptables -X dsl-Ab12cd34ef56-$k"));
        // `none`: nothing but the lane's own bridges.
        let s = apply_script("0a1b2c3d", "Ab-12cd34ef56gh78", &nets, Network::None);
        assert!(s.contains(&format!("iptables -A {f} -j DROP")));
        assert!(!s.contains("192.168.0.0/16"));
    }

    #[test]
    fn a_sweep_removes_only_its_own_agents_rules() {
        let s = cleanup_script("0a1b2c3d", &["aaaa1111".into()], &["bbbb2222".into()]);
        // Its own chains, by its owner tag; not another agent's.
        assert!(s.contains("s/^-N \\(ds-0a1b2c3d-[A-Za-z0-9]*\\)-f$/\\1/p"));
        assert!(!s.contains("ds-[A-Za-z0-9]"));
        assert!(s.contains("ds-0a1b2c3d-aaaa1111-f"));
        // An older agent's chains: kept while any lane container has the
        // lease, or this agent holds it.
        assert!(s.contains("s/^-N \\(dsl-[A-Za-z0-9]*\\)-f$/\\1/p"));
        assert!(s.contains("dsl-aaaa1111-f") && s.contains("dsl-bbbb2222-f"));
    }
}
