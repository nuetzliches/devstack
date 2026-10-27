//! A lane's infrastructure on a node's Docker engine, from its start to its
//! purge (ADR-0033, ADR-0038): what the lane's files ask runs as the agent
//! checked it, nothing of the agent's environment reaches it, a stop leaves
//! no container and a purge no volume.
//!
//! Opt-in (`#[ignore]`d): `DEVSTACK_DOCKER_TESTS=1` and `-- --include-ignored`,
//! on Linux (the mirror is mounted at its own path in Linux containers, as on
//! a node).

#[cfg(unix)]
#[test]
#[ignore = "needs Docker on Linux: DEVSTACK_DOCKER_TESTS=1, --include-ignored"]
fn a_lane_stops_and_is_purged_with_its_infrastructure() {
    use devstack_node::lanerun::LaneRun;
    use devstack_node::policy::Network;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    if std::env::var("DEVSTACK_DOCKER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set DEVSTACK_DOCKER_TESTS=1");
        return;
    }
    // What the agent's environment holds is not the lane's to read.
    // SAFETY: this test binary runs this one test.
    unsafe { std::env::set_var("DSL_AGENT_SECRET", "of-the-agent") };

    let docker = |args: &[&str]| -> String {
        let out = Command::new("docker").args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let d = tempfile::tempdir().unwrap();
    let tag = format!("dsl{}", std::process::id());
    let dir = d.path().join("slots").join(&tag);
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("data")).unwrap();
    std::fs::create_dir_all(src.join("api")).unwrap();
    std::fs::write(
        src.join("devstack.toml"),
        "version = 1\n\n[stack]\nname = \"t\"\n\n[lanes]\nimage = \"busybox:1.37\"\n\n\
         [compose.app]\nproject = \"dsltest\"\nfiles = [\"compose.yml\"]\nservices = [\"db\"]\n",
    )
    .unwrap();
    std::fs::write(
        src.join("compose.yml"),
        // `api` is the stack's own service: the lane does not start it.
        r#"services:
  api:
    image: busybox:1.37
    profiles: [container-app]
    ports: ["8080:80"]
    depends_on: [db]
  db:
    image: busybox:1.37
    command: ["sh", "-c", "trap 'exit 0' TERM; while true; do sleep 1; done"]
    environment: [DSL_AGENT_SECRET]
    volumes: ["./data:/data", "keep:/keep"]
volumes:
  keep: {}
"#,
    )
    .unwrap();
    // The lane's devstack: writes its token, then waits.
    let bin = d.path().join("devstack");
    std::fs::write(
        &bin,
        "#!/bin/sh\nmkdir -p \"$XDG_STATE_HOME/devstack\"\necho 0123abcd > \"$XDG_STATE_HOME/devstack/token\"\n\
         trap 'exit 0' TERM\nwhile true; do sleep 1; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let run = LaneRun {
        id: tag.clone(),
        slot: tag.clone(),
        member: "m".into(),
        lane: "ci".into(),
        dir: dir.clone(),
        stack_file: "devstack.toml".into(),
        display_root: None,
        port_offset: 0,
        profile: None,
        relay_udp: false,
        workers: false,
        memory_limit: Some(256 << 20),
        cpu_limit: None,
        network: Network::Lan,
        secret: "s".into(),
        store: None,
        warm: false,
    };
    let project = format!("dsltest-{tag}");
    let label = format!("label=com.docker.compose.project={project}");
    let started = run.start(&bin, None, &|p| eprintln!("phase: {p}"));
    let started = match started {
        Ok(a) => a,
        Err(e) => {
            run.purge(None);
            panic!("start: {e}");
        }
    };
    assert_eq!(started.addr.token, "0123abcd");
    // The daemon answers before its containers run (#164): it reads them
    // as created, and the agent brings them up after.
    let lane_file = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(src.join(".devstack/external-infra.json")).unwrap())
            .unwrap()
    };
    let infra = lane_file();
    assert_eq!(infra["groups"]["app"]["state"], "created", "{infra}");
    assert_eq!(infra["groups"]["app"]["services"]["db"]["state"], "created");
    assert_eq!(
        docker(&["ps", "-q", "--filter", &label]),
        "",
        "nothing runs yet"
    );
    if let Err(e) = started.infra.run(&run, &|| false, &|_, _, _| {}) {
        run.purge(None);
        panic!("bringing up: {e}");
    }
    let infra = lane_file();
    assert_eq!(infra["groups"]["app"]["state"], "up", "{infra}");
    assert_eq!(infra["groups"]["app"]["services"]["db"]["state"], "running");
    let db = docker(&["ps", "-q", "--filter", &label]);
    assert!(!db.is_empty(), "the lane's db runs");
    // The mirror is mounted as a subpath of a volume, never as a bind
    // mount a link in the mirror could redirect.
    let mounts = docker(&["inspect", "-f", "{{range .Mounts}}{{.Type}} {{end}}", &db]);
    assert!(!mounts.contains("bind"), "{mounts}");
    let env = docker(&["inspect", "-f", "{{range .Config.Env}}{{.}} {{end}}", &db]);
    assert!(!env.contains("of-the-agent"), "{env}");

    // Stopped: no container of the lane is left, its data is.
    run.stop(None);
    assert_eq!(docker(&["ps", "-aq", "--filter", &label]), "");
    assert_eq!(
        docker(&[
            "ps",
            "-aq",
            "--filter",
            &format!("label=devstack.lease={tag}")
        ]),
        ""
    );
    assert!(!docker(&["volume", "ls", "-q", "--filter", &label]).is_empty());

    // Started again, stopped while its containers come up: none of them
    // starts after the stop (#164).
    let again = run.start(&bin, None, &|_| {}).unwrap();
    let stopped = std::sync::atomic::AtomicBool::new(true);
    again
        .infra
        .run(
            &run,
            &|| stopped.load(std::sync::atomic::Ordering::SeqCst),
            &|_, _, _| {},
        )
        .unwrap();
    assert_eq!(docker(&["ps", "-q", "--filter", &label]), "");
    // And purged: nothing of it is left on the engine.
    run.purge(None);
    for what in [
        &["ps", "-aq", "--filter", &label][..],
        &["volume", "ls", "-q", "--filter", &label][..],
        &["network", "ls", "-q", "--filter", &label][..],
    ] {
        assert_eq!(docker(what), "", "docker {}", what.join(" "));
    }
    assert!(!dir.exists());
}
