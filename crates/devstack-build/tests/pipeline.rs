//! Snapshot, build id, build, publish, cancel and prune against a small git repository.

use devstack_build::{Builder, Layout, Outcome, Snapshot, build_id};
use devstack_config::model::Service;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    fs::create_dir_all(d.path().join("src/feature")).unwrap();
    fs::write(d.path().join("src/a.txt"), "a").unwrap();
    fs::write(d.path().join("src/feature/b.txt"), "b").unwrap();
    fs::write(d.path().join("docs.md"), "docs").unwrap();
    fs::write(d.path().join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir_all(d.path().join("ignored")).unwrap();
    fs::write(d.path().join("ignored/x"), "x").unwrap();
    git(d.path(), &["add", "src/a.txt"]); // tracked and untracked files both count
    d
}

fn service(toml: &str) -> Service {
    let text = format!("version = 1\n[stack]\nname = \"t\"\n[services.s]\n{toml}");
    devstack_config::from_str(&text, Path::new("t"))
        .unwrap()
        .services
        .remove("s")
        .unwrap()
}

/// Builds run again and again in the same snapshot, so the command must be
/// repeatable (as `dotnet build` and `ng build` are).
#[cfg(windows)]
const OK_BUILD: &str = "build = \"if not exist out mkdir out && git --version > out/version.txt\"
output = \"out\"
run = \"x\"
";
#[cfg(not(windows))]
const OK_BUILD: &str = "build = \"mkdir -p out && git --version > out/version.txt\"
output = \"out\"
run = \"x\"
";

#[test]
fn snapshot_copies_changes_only_and_stamps_them() {
    let r = repo();
    let state = tempfile::tempdir().unwrap();
    let mut snap = Snapshot::open(r.path(), &state.path().join("lane")).unwrap();
    let s1 = snap.sync().unwrap();
    assert_eq!(
        (s1.files, s1.copied, s1.removed),
        (4, 4, 0),
        "a, b, docs.md, .gitignore; not ignored/"
    );
    assert!(!snap.dir.join("ignored/x").exists());

    assert_eq!(snap.sync().unwrap().copied, 0, "no-op sync copies nothing");

    // A restored file keeps an old mtime; its copy must still be newer than any output.
    let old = SystemTime::now() - Duration::from_secs(3600);
    fs::write(r.path().join("src/a.txt"), "a2").unwrap();
    fs::File::options()
        .write(true)
        .open(r.path().join("src/a.txt"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    fs::remove_file(r.path().join("docs.md")).unwrap();
    let before = SystemTime::now() - Duration::from_secs(5);
    let s3 = snap.sync().unwrap();
    assert_eq!((s3.copied, s3.removed), (1, 1));
    assert_eq!(
        fs::read_to_string(snap.dir.join("src/a.txt")).unwrap(),
        "a2"
    );
    assert!(
        fs::metadata(snap.dir.join("src/a.txt"))
            .unwrap()
            .modified()
            .unwrap()
            > before
    );
    assert!(!snap.dir.join("docs.md").exists());

    // The manifest survives a reopen: nothing to copy.
    let mut again = Snapshot::open(r.path(), &state.path().join("lane")).unwrap();
    assert_eq!(again.sync().unwrap().copied, 0);
}

#[test]
fn build_id_follows_watched_inputs_and_the_build_definition() {
    let r = repo();
    let state = tempfile::tempdir().unwrap();
    let mut snap = Snapshot::open(r.path(), &state.path().join("lane")).unwrap();
    snap.sync().unwrap();
    let watched = service(&format!("{OK_BUILD}watch = [\"src/**\"]\n"));
    let id1 = build_id(&snap.manifest, &watched).unwrap();

    fs::write(r.path().join("docs.md"), "changed docs").unwrap();
    snap.sync().unwrap();
    assert_eq!(
        build_id(&snap.manifest, &watched).unwrap(),
        id1,
        "unwatched change keeps the id"
    );

    fs::write(r.path().join("src/feature/b.txt"), "b2").unwrap();
    snap.sync().unwrap();
    let id2 = build_id(&snap.manifest, &watched).unwrap();
    assert_ne!(id2, id1, "watched change gives a new id");

    let other_cmd = service(&format!(
        "{}watch = [\"src/**\"]\n",
        OK_BUILD.replace("version.txt", "v.txt")
    ));
    assert_ne!(
        build_id(&snap.manifest, &other_cmd).unwrap(),
        id2,
        "a changed build command gives a new id"
    );
}

#[test]
fn build_publish_failure_cancel_and_prune() {
    let r = repo();
    let state = tempfile::tempdir().unwrap();
    let layout = Layout::new(state.path());
    let mut snap = Snapshot::open(r.path(), &layout.lane("main")).unwrap();
    snap.sync().unwrap();
    let builder = Builder::new(layout.clone(), &Default::default()).unwrap();

    // Success: published, complete, with metadata.
    let ok = service(OK_BUILD);
    let mut run = builder.start("main", &snap, "s", &ok).unwrap();
    let out = builder.wait(&mut run).unwrap();
    let Outcome::Published { id, dir, .. } = out else {
        panic!("{out:?}")
    };
    assert!(
        fs::read_to_string(dir.join("version.txt"))
            .unwrap()
            .contains("git version")
    );
    assert_eq!(builder.artifact("s", &id).unwrap().lane, "main");

    // Failure: nothing published, no temporary directory left.
    let failing = service("build = \"exit 4\"\noutput = \"out\"\nrun = \"x\"\n");
    let mut run = builder.start("main", &snap, "f", &failing).unwrap();
    assert!(matches!(
        builder.wait(&mut run).unwrap(),
        Outcome::Failed { exit_code: 4, .. }
    ));
    assert!(
        !layout.artifacts("f").exists()
            || fs::read_dir(layout.artifacts("f"))
                .unwrap()
                .next()
                .is_none()
    );

    // Cancel: superseded build publishes nothing.
    let slow_cmd = if cfg!(windows) {
        "ping -n 30 127.0.0.1 > nul && mkdir out"
    } else {
        "sleep 30 && mkdir out"
    };
    let slow = service(&format!(
        "build = \"{slow_cmd}\"\noutput = \"out\"\nrun = \"x\"\n"
    ));
    let mut run = builder.start("main", &snap, "c", &slow).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(matches!(
        builder.cancel(&mut run),
        Outcome::Cancelled { .. }
    ));
    assert!(!layout.artifact("c", &run.id).exists());

    // Prune keeps the newest and the pinned.
    for n in 0..3 {
        let svc = service(&OK_BUILD.replace("version.txt", &format!("v{n}.txt")));
        let mut run = builder.start("main", &snap, "p", &svc).unwrap();
        assert!(matches!(
            builder.wait(&mut run).unwrap(),
            Outcome::Published { .. }
        ));
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut ids: Vec<String> = fs::read_dir(layout.artifacts("p"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort_by_key(|id| builder.artifact("p", id).unwrap().created);
    let oldest = ids[0].clone();
    let removed = builder.prune("p", 1, &[&oldest]);
    assert_eq!(removed.len(), 1, "3 artifacts, keep 1 newest + 1 pinned");
    assert!(layout.artifact("p", &oldest).exists());
}

#[test]
fn a_half_published_artifact_is_removed_on_start() {
    let state = tempfile::tempdir().unwrap();
    let layout = Layout::new(state.path());
    let tmp = layout.artifacts("s").join(".tmp-abc");
    fs::create_dir_all(&tmp).unwrap();
    let _b = Builder::new(layout, &Default::default()).unwrap();
    assert!(!tmp.exists());
}
