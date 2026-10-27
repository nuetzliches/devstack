//! A pool grows by nodes a provisioner makes (ADR-0051), against the mock
//! provisioner: a lane no node can take waits for a machine, which enrols
//! once from its address and is offered; it is filled, told to the
//! provisioner, and given back when idle. Enrolment alarms, refusals, caps
//! and an admin's confirmation.

use devstack_node::coordinator::{self, LeaseRequest, Leasing, Options, Report};
use devstack_node::member::Local;
use devstack_node::provision::mock::{Behaviour, Mock};
use devstack_node::provision::{Confirm, MachineRequest, Provisioner, Settings};
use devstack_node::share::Need;
use devstack_node::store::{SHELL, Store};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A coordinator in `dir` with the provisioner at `url`, looking after its
/// machines every 100 ms.
fn start(dir: &Path, url: &str) -> (String, String) {
    let fp = coordinator::init(dir, &["127.0.0.1".into()]).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let coord = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let (dir, options) = (
        dir.to_path_buf(),
        Options {
            provisioner: Some(Provisioner::new(dir, url)),
            tend_every: Duration::from_millis(100),
        },
    );
    std::thread::spawn(move || {
        rt.block_on(coordinator::serve_with(
            &dir,
            listener,
            std::future::pending(),
            options,
        ))
        .unwrap();
    });
    (coord, fp)
}

fn until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(secs), "never: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn node_report(leases: Vec<coordinator::LeaseReport>) -> Report {
    Report {
        sharing: true,
        cpus: 8.0,
        memory: 16 << 30,
        api: Some("https://127.0.0.1:1".into()),
        dedicated: true,
        leases,
        ..Report::default()
    }
}

fn ask(lane: &str) -> LeaseRequest {
    LeaseRequest {
        node: "pool".into(),
        repo: "shop".into(),
        lane: lane.into(),
        need: Need {
            cpus: 2.0,
            memory: 4 << 30,
            ..Need::default()
        },
        need_from: None,
        floor: None,
        profile: None,
        devstack: None,
        warm: false,
    }
}

/// The machines the hook enrolled, by their membership directory.
type Enrolled = Arc<Mutex<Vec<PathBuf>>>;

/// A mock whose machines enrol (and report as nodes) from this machine.
fn enrolling(behaviour: Behaviour, root: &Path) -> (Mock, Enrolled) {
    let enrolled: Enrolled = Arc::default();
    let (e, root) = (enrolled.clone(), root.to_path_buf());
    let mock = Mock::start(
        behaviour,
        Some(Box::new(move |r: MachineRequest, machine: String| {
            let dir = root.join(&machine);
            let l = Local::new(&dir);
            let t = &r.enrol;
            if l.enrol(&t.coordinator, &t.fingerprint, &machine, &t.token)
                .is_ok()
            {
                l.report(&node_report(Vec::new())).unwrap();
            }
            e.lock().unwrap().push(dir);
        })),
    );
    (mock, enrolled)
}

/// A member of the pool at `url`, approved, in a directory of its own.
fn member(url: &str, fp: &str, coord: &Path, name: &str, dir: &Path) -> (Local, String) {
    let l = Local::new(dir);
    let m = l.join(url, fp, name).unwrap();
    Store::new(coord)
        .approve(&m.id, &coordinator::ca(coord).unwrap(), None, SHELL)
        .unwrap();
    l.refresh().unwrap();
    (l, m.id)
}

fn offer_of(l: &Local, first: Leasing) -> Result<coordinator::LeaseOffer, String> {
    let mut at = first;
    let t = Instant::now();
    loop {
        match at {
            Leasing::Offer(o) => return Ok(o),
            Leasing::Provisioning(p) => {
                assert!(t.elapsed() < Duration::from_secs(20), "still: {}", p.why);
                std::thread::sleep(Duration::from_millis(100));
                at = l.wait(&p.id).map_err(|e| e.to_string())?;
            }
        }
    }
}

#[test]
fn a_lane_no_node_takes_waits_for_a_machine_that_is_filled_and_given_back() {
    let coord = tempfile::tempdir().unwrap();
    let machines = tempfile::tempdir().unwrap();
    let (mock, enrolled) = enrolling(
        Behaviour {
            fail_deletes: 1,
            ..Behaviour::default()
        },
        machines.path(),
    );
    let (url, fp) = start(coord.path(), &mock.url);
    let store = Store::new(coord.path());
    Settings {
        idle_minutes: 0,
        ..Settings::default()
    }
    .save(coord.path())
    .unwrap();
    let pc = tempfile::tempdir().unwrap();
    let (dev, dev_id) = member(&url, &fp, coord.path(), "dev-pc", pc.path());
    store.set_provision(&dev_id, true, SHELL).unwrap();

    // Without `?wait=provision` (an older requester): refused as before.
    let e = dev.lease(&ask("a")).unwrap_err().to_string();
    assert!(e.contains("no node of the pool"), "{e}");
    assert!(mock.seen().requests.is_empty());

    let first = dev.lease_or_wait(&ask("a")).unwrap();
    let Leasing::Provisioning(p) = &first else {
        panic!("offered without a node: {first:?}");
    };
    assert_eq!(p.state, "provisioning");
    let offer = offer_of(&dev, first).unwrap();
    assert!(offer.provisioned);
    assert_eq!(offer.node.name, "mock-1");
    assert!(
        offer.why.as_deref().unwrap().contains("mock-1"),
        "{offer:?}"
    );
    let asked = &mock.seen().requests[0];
    assert_eq!(asked.need.cpus, 2.0);
    assert_eq!(asked.enrol.coordinator, url);
    assert!(asked.enrol.command.contains("--enrol"));

    // A second lane gathers on it (filled), no new machine is made.
    let node = Local::new(&enrolled.lock().unwrap()[0]);
    let second = offer_of(&dev, dev.lease_or_wait(&ask("b")).unwrap()).unwrap();
    assert_eq!(second.node.id, offer.node.id);
    assert_eq!(mock.seen().requests.len(), 1);

    // The node runs both: begun; then nothing: ended, idle, given back.
    let running = |id: &str, lane: &str| coordinator::LeaseReport {
        id: id.into(),
        member: dev_id.clone(),
        repo: "shop".into(),
        lane: lane.into(),
        expires_in: 60,
        need: None,
    };
    node.report(&node_report(vec![
        running(&offer.id, "a"),
        running(&second.id, "b"),
    ]))
    .unwrap();
    until("lease_begun", 10, || {
        mock.seen().events.iter().any(|e| e.kind == "lease_begun")
    });
    let nodes = dev.nodes().unwrap();
    let pv = nodes
        .iter()
        .find(|n| n.id == offer.node.id)
        .and_then(|n| n.provisioned.clone())
        .unwrap();
    assert_eq!(pv.machine, "mock-1");
    node.report(&node_report(Vec::new())).unwrap();
    until("node_returned", 10, || {
        mock.seen().events.iter().any(|e| e.kind == "node_returned")
    });
    let seen = mock.seen();
    let kinds: Vec<&str> = seen.events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds[0], "node_ready");
    assert!(kinds.contains(&"lease_ended"), "{kinds:?}");
    assert_eq!(seen.deleted, ["mock-1"]);
    assert_eq!(seen.failed_deletes, 1, "a failed deletion is retried");
    assert!(node.nodes().is_err(), "its membership is revoked");
}

#[test]
fn provisioning_is_refused_with_its_reason() {
    let coord = tempfile::tempdir().unwrap();
    let mock = Mock::start(
        Behaviour {
            refuse: Some("no capacity in the region".into()),
            ..Behaviour::default()
        },
        None,
    );
    let (url, fp) = start(coord.path(), &mock.url);
    let store = Store::new(coord.path());
    let pc = tempfile::tempdir().unwrap();
    let (dev, dev_id) = member(&url, &fp, coord.path(), "dev-pc", pc.path());

    let e = dev.lease_or_wait(&ask("a")).unwrap_err().to_string();
    assert!(e.contains("may not cause provisioning"), "{e}");
    store.set_provision(&dev_id, true, SHELL).unwrap();

    dev.set_provisioned(false).unwrap();
    let e = dev.lease_or_wait(&ask("a")).unwrap_err().to_string();
    assert!(e.contains("keep your lanes off"), "{e}");
    dev.set_provisioned(true).unwrap();
    assert!(mock.seen().requests.is_empty());

    let e = dev.lease_or_wait(&ask("a")).unwrap_err().to_string();
    assert!(e.contains("the provisioner refused: no capacity"), "{e}");

    Settings {
        max_nodes: 0,
        ..Settings::default()
    }
    .save(coord.path())
    .unwrap();
    let e = dev.lease_or_wait(&ask("a")).unwrap_err().to_string();
    assert!(e.contains("at most 0"), "{e}");
    assert_eq!(mock.seen().requests.len(), 1);
}

#[test]
fn a_second_use_of_an_enrolment_token_is_an_alarm() {
    let coord = tempfile::tempdir().unwrap();
    let machines = tempfile::tempdir().unwrap();
    let root = machines.path().to_path_buf();
    let outcome: Arc<Mutex<Vec<String>>> = Arc::default();
    let o = outcome.clone();
    let mock = Mock::start(
        Behaviour::default(),
        Some(Box::new(move |r: MachineRequest, machine: String| {
            let t = &r.enrol;
            // The machine, then a thief with the same token.
            for who in ["machine", "thief"] {
                let l = Local::new(&root.join(format!("{machine}-{who}")));
                let got = l.enrol(&t.coordinator, &t.fingerprint, who, &t.token);
                o.lock().unwrap().push(format!(
                    "{who}: {}",
                    got.map_or_else(|e| e.to_string(), |_| "in".into())
                ));
            }
        })),
    );
    let (url, fp) = start(coord.path(), &mock.url);
    let pc = tempfile::tempdir().unwrap();
    let (dev, dev_id) = member(&url, &fp, coord.path(), "dev-pc", pc.path());
    Store::new(coord.path())
        .set_provision(&dev_id, true, SHELL)
        .unwrap();

    let first = dev.lease_or_wait(&ask("a")).unwrap();
    let e = offer_of(&dev, first).unwrap_err();
    assert!(e.contains("given back"), "{e}");
    let out = outcome.lock().unwrap().clone();
    assert_eq!(out[0], "machine: in");
    assert!(out[1].contains("refused"), "{out:?}");
    until("deleted", 10, || mock.seen().deleted == ["mock-1"]);
    let audit = Store::new(coord.path()).audit(10).unwrap();
    let alarm = audit.iter().find(|a| a.action == "alarm").unwrap();
    assert!(
        alarm.detail.as_deref().unwrap().contains("used again"),
        "{alarm:?}"
    );
    let removed = audit.iter().find(|a| a.action == "remove").unwrap();
    assert_eq!(removed.name, "machine");
}

#[test]
fn an_enrolment_from_another_address_is_refused_and_an_alarm() {
    let coord = tempfile::tempdir().unwrap();
    let machines = tempfile::tempdir().unwrap();
    let (mock, enrolled) = enrolling(
        Behaviour {
            address: "10.9.8.7".into(),
            ..Behaviour::default()
        },
        machines.path(),
    );
    let (url, fp) = start(coord.path(), &mock.url);
    let pc = tempfile::tempdir().unwrap();
    let (dev, dev_id) = member(&url, &fp, coord.path(), "dev-pc", pc.path());
    Store::new(coord.path())
        .set_provision(&dev_id, true, SHELL)
        .unwrap();
    let e = offer_of(&dev, dev.lease_or_wait(&ask("a")).unwrap()).unwrap_err();
    assert!(e.contains("given back"), "{e}");
    // The mock records the machine once its refused enrolment returned,
    // which may be after the lease was given back.
    until("enrolled", 10, || !enrolled.lock().unwrap().is_empty());
    assert_eq!(enrolled.lock().unwrap().len(), 1);
    let l = Local::new(&enrolled.lock().unwrap()[0]);
    assert!(l.nodes().is_err(), "it never joined");
    until("deleted", 10, || mock.seen().deleted == ["mock-1"]);
}

#[test]
fn an_admin_confirms_a_provisioned_node_where_the_pool_asks_for_it() {
    let coord = tempfile::tempdir().unwrap();
    let machines = tempfile::tempdir().unwrap();
    let (mock, enrolled) = enrolling(Behaviour::default(), machines.path());
    let (url, fp) = start(coord.path(), &mock.url);
    Settings {
        confirm: Confirm::Admin,
        per: devstack_node::provision::Per::Repo,
        ..Settings::default()
    }
    .save(coord.path())
    .unwrap();
    let store = Store::new(coord.path());
    let pc = tempfile::tempdir().unwrap();
    let (dev, dev_id) = member(&url, &fp, coord.path(), "dev-pc", pc.path());
    store.set_provision(&dev_id, true, SHELL).unwrap();

    let Leasing::Provisioning(p) = dev.lease_or_wait(&ask("a")).unwrap() else {
        panic!("offered");
    };
    until("enrolled", 10, || !enrolled.lock().unwrap().is_empty());
    let mut why = String::new();
    until("waiting for an admin", 10, || {
        match dev.wait(&p.id).unwrap() {
            Leasing::Provisioning(p) => why = p.why,
            Leasing::Offer(o) => panic!("offered unconfirmed: {o:?}"),
        }
        why.contains("confirm")
    });
    let node = store
        .list()
        .unwrap()
        .into_iter()
        .find(|m| m.provisioned.is_some())
        .unwrap();
    // Bound to the repository it was made for, by its policy.
    assert_eq!(node.policy.as_ref().unwrap().repos, ["shop"]);
    store.confirm(&node.id, SHELL).unwrap();
    let offer = offer_of(&dev, dev.wait(&p.id).unwrap()).unwrap();
    assert_eq!(offer.node.id, node.id);
}
