//! Against a real Docker engine. Opt-in (`#[ignore]`d): `DEVSTACK_DOCKER_TESTS=1`
//! and `-- --include-ignored` (Linux containers required; CI runs it on Linux
//! only).

use devstack_compose::{Docker, Group};
use std::time::Duration;

#[test]
#[ignore = "needs Docker with Linux containers: DEVSTACK_DOCKER_TESTS=1, --include-ignored"]
fn up_ps_published_ports_and_parked_services() {
    if std::env::var("DEVSTACK_DOCKER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set DEVSTACK_DOCKER_TESTS=1");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("deploy.yml"),
        r#"services:
  api:
    image: busybox:1.37
    environment: { DB_URL: "http://db:5432/x", MODE: "${MODE:-prod}" }
    depends_on: [db]
  db:
    image: busybox:1.37
    command: ["sh", "-c", "trap 'exit 0' TERM; while true; do sleep 1; done"]
    healthcheck: { test: ["CMD", "true"], interval: 1s }
"#,
    )
    .unwrap();
    // The dev overlay parks the application container and publishes the database.
    std::fs::write(dir.path().join("dev.yml"), "services:\n  api:\n    profiles: [container-app]\n  db:\n    ports: [\"127.0.0.1::5432\"]\n").unwrap();
    std::fs::write(dir.path().join(".env"), "MODE=dev\n").unwrap();
    let project = format!("devstack-test-{}", std::process::id());
    let g = Group {
        name: "app".into(),
        project: project.clone(),
        files: vec![dir.path().join("deploy.yml"), dir.path().join("dev.yml")],
        env_files: vec![dir.path().join(".env")],
        networks: vec![format!("{project}-edge")],
        services: vec!["db".into()],
        dir: dir.path().into(),
        env: Default::default(),
    };
    let docker = Docker::default();
    docker.validate(&g).unwrap();

    // The parked service is visible to config (ADR-0014) with the env file applied.
    let model = docker.config(&g).unwrap();
    assert_eq!(
        model.services["api"].environment["MODE"].as_deref(),
        Some("dev")
    );

    let result = (|| {
        docker.up(&g, Duration::from_secs(60))?;
        let ps = docker.ps(&g)?;
        Ok::<_, devstack_compose::ComposeError>(ps)
    })();
    let ps = result.as_ref().map(|v| v.clone());
    let _ = docker.down(&g);
    let _ = std::process::Command::new("docker")
        .args(["network", "rm", &format!("{project}-edge")])
        .output();

    let ps = ps.unwrap();
    assert_eq!(
        ps.len(),
        1,
        "only the selected service runs (--no-deps, parked api): {ps:?}"
    );
    assert!(ps[0].is_ready());
    let addr = ps[0].published(5432).expect("db publishes 5432");
    assert!(addr.starts_with("127.0.0.1:"), "{addr}");
}
