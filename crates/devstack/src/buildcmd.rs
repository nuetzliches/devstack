//! `devstack build <service>`: sync the lane, build, publish (ADR-0002). The
//! daemon will do the same on every change (ADR-0003); this command runs one
//! build by hand.

use anyhow::{Context, Result, bail};
use devstack_build::{Builder, Layout, Outcome, Snapshot, build_id};
use devstack_config::Stack;
use std::path::Path;

pub fn run(stack: &Stack, repo: &Path, name: &str, lane: &str) -> Result<()> {
    let svc = stack
        .services
        .get(name)
        .with_context(|| format!("no service `{name}` in the stack file"))?;
    if svc.build.is_none() {
        bail!("service `{name}` has no `build`");
    }
    warn_if_state_is_not_ignored(repo);
    let layout = Layout::new(repo.join(".devstack"));
    // Lanes other than `main` come with worktree support (phase 3).
    if lane != "main" {
        bail!("only the `main` lane exists so far");
    }
    let mut snap = Snapshot::open(repo, &layout.lane(lane))?;
    let s = snap.sync()?;
    eprintln!(
        "snapshot {lane}: {} file(s), {} copied, {} removed, {} ms",
        s.files,
        s.copied,
        s.removed,
        s.elapsed.as_millis()
    );

    let id = build_id(&snap.manifest, svc)?;
    let exe = std::env::current_exe()?;
    let opts = devstack_proc::Options {
        cgroup_root: None,
        shim: vec![exe.to_string_lossy().into_owned(), "shim".into()],
    };
    let builder = Builder::new(layout.clone(), &opts)?;
    if builder.artifact(name, &id).is_some() {
        println!(
            "{name}: up to date, artifact {id}\n  {}",
            layout.artifact(name, &id).display()
        );
        return Ok(());
    }
    eprintln!(
        "building {name} ({id}), log: {}",
        layout.build_log(name, &id).display()
    );
    let mut running = builder.start(lane, &snap, name, svc)?;
    match builder.wait(&mut running)? {
        Outcome::Published { id, dir, elapsed } => {
            println!(
                "{name}: built {id} in {:.1} s\n  {}",
                elapsed.as_secs_f64(),
                dir.display()
            );
            Ok(())
        }
        Outcome::Failed { exit_code, log, .. } => {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(25).collect();
            eprintln!("{}", tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
            bail!(
                "build of `{name}` failed with exit code {exit_code}; full log: {}",
                log.display()
            )
        }
        Outcome::Cancelled { .. } => bail!("build of `{name}` was cancelled"),
        Outcome::Stale { .. } => bail!("the sources of `{name}` changed while it built; try again"),
    }
}

/// `.devstack/` must not be committed, and the snapshot already skips it.
fn warn_if_state_is_not_ignored(repo: &Path) {
    let ignored = std::process::Command::new("git")
        .args(["check-ignore", "-q", ".devstack/"])
        .current_dir(repo)
        .status()
        .is_ok_and(|s| s.success());
    if !ignored {
        eprintln!("note: add `.devstack/` to .gitignore; it holds snapshots, artifacts and logs");
    }
}
