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

/// #13: build files above the watched files are inputs (MSBuild's
/// `Directory.Build.targets` at the root); build files beside other projects
/// are not.
#[test]
fn build_files_above_the_watched_ones_count() {
    let r = repo();
    let state = tempfile::tempdir().unwrap();
    let mut snap = Snapshot::open(r.path(), &state.path().join("lane")).unwrap();
    fs::create_dir_all(r.path().join("web")).unwrap();
    fs::write(r.path().join("Directory.Build.targets"), "<Project />").unwrap();
    fs::write(r.path().join("web/package.json"), "{}").unwrap();
    snap.sync().unwrap();
    let watched = service(&format!(
        "{OK_BUILD}watch = [\"src/feature/**\"]
"
    ));
    let id1 = build_id(&snap.manifest, &watched).unwrap();

    fs::write(
        r.path().join("Directory.Build.targets"),
        "<Project><Target Name=\"X\" /></Project>",
    )
    .unwrap();
    snap.sync().unwrap();
    let id2 = build_id(&snap.manifest, &watched).unwrap();
    assert_ne!(id2, id1, "a build file at the root is an input");

    fs::write(r.path().join("web/package.json"), "{\"name\":\"web\"}").unwrap();
    snap.sync().unwrap();
    assert_eq!(
        build_id(&snap.manifest, &watched).unwrap(),
        id2,
        "a build file beside another project is not"
    );
    assert!(devstack_build::is_build_file("x/NuGet.Config"));
    assert!(!devstack_build::is_build_file("src/Program.cs"));
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

    // A directory without metadata under the id (a prune that stopped
    // halfway) is no artifact: the next build publishes it afresh.
    fs::remove_file(dir.join(devstack_build::META_FILE)).unwrap();
    fs::remove_file(dir.join("version.txt")).unwrap();
    let mut run = builder.start("main", &snap, "s", &ok).unwrap();
    assert!(matches!(
        builder.wait(&mut run).unwrap(),
        Outcome::Published { .. }
    ));
    assert!(dir.join("version.txt").is_file(), "republished complete");
    assert!(builder.artifact("s", &id).is_some());

    // Inputs that changed while it built: nothing is published under the
    // id, and an artifact of that id that exists stays as it is.
    let other = service(&OK_BUILD.replace("version.txt", "stale.txt"));
    let mut run = builder.start("main", &snap, "st", &other).unwrap();
    let out = loop {
        if let Some(o) = builder.poll_checked(&mut run, &|| false).unwrap() {
            break o;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(matches!(out, Outcome::Stale { .. }), "{out:?}");
    assert!(builder.artifact("st", &run.id).is_none());
    assert!(
        fs::read_dir(layout.artifacts("st"))
            .map(|mut d| d.next().is_none())
            .unwrap_or(true),
        "no temporary directory left"
    );
    let mut run = builder.start("main", &snap, "s", &ok).unwrap();
    let out = loop {
        if let Some(o) = builder.poll_checked(&mut run, &|| false).unwrap() {
            break o;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(matches!(out, Outcome::Stale { .. }), "{out:?}");
    assert!(
        builder.artifact("s", &id).is_some(),
        "the existing one stays"
    );

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
    // Leftovers of an unfinished prune go with the next one.
    let trash = layout.artifacts("p").join(".trash-old");
    fs::create_dir_all(trash.join("x")).unwrap();
    builder.prune("p", 1, &[&oldest]);
    assert!(!trash.exists());
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

/// F-28: where a build runs and what prepares it are part of how it is built.
#[test]
fn build_id_covers_cwd_and_prepare() {
    let r = repo();
    let state = tempfile::tempdir().unwrap();
    let mut snap = Snapshot::open(r.path(), &state.path().join("lane")).unwrap();
    snap.sync().unwrap();
    let id =
        |extra: &str| build_id(&snap.manifest, &service(&format!("{OK_BUILD}{extra}"))).unwrap();
    let base = id("watch = [\"src/**\"]\n");
    let ids = [
        id("watch = [\"src/**\"]\ncwd = \"src\"\n"),
        id("watch = [\"src/**\"]\ncwd = \"src/feature\"\n"),
        id("watch = [\"src/**\"]\nprepare = { run = \"npm ci\" }\n"),
        id("watch = [\"src/**\"]\nprepare = { run = \"npm install\" }\n"),
        id("watch = [\"src/**\"]\nprepare = { run = \"npm ci\", cwd = \"src\" }\n"),
        id("watch = [\"src/**\"]\nprepare = { run = \"npm ci\", inputs = [\"docs.md\"] }\n"),
    ];
    let mut all: Vec<&String> = ids.iter().collect();
    all.push(&base);
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 7, "every variant has its own id: {ids:?} {base}");

    // The content of `prepare.inputs` counts, though `watch` does not match it.
    let with_inputs =
        "watch = [\"src/**\"]\nprepare = { run = \"npm ci\", inputs = [\"docs.md\"] }\n";
    let before = id(with_inputs);
    fs::write(r.path().join("docs.md"), "new lockfile").unwrap();
    snap.sync().unwrap();
    let after = build_id(
        &snap.manifest,
        &service(&format!("{OK_BUILD}{with_inputs}")),
    )
    .unwrap();
    assert_ne!(before, after);
}

/// F-27: removals come before copies, and a path may change its type.
#[test]
fn snapshot_follows_renames_and_type_changes() {
    let r = repo();
    let state = tempfile::tempdir().unwrap();
    let mut snap = Snapshot::open(r.path(), &state.path().join("lane")).unwrap();
    fs::write(r.path().join("Foo.cs"), "foo").unwrap();
    fs::write(r.path().join("node"), "a file").unwrap();
    fs::create_dir_all(r.path().join("tree/sub")).unwrap();
    fs::write(r.path().join("tree/sub/x"), "x").unwrap();
    snap.sync().unwrap();

    // Case-only rename, file -> directory, directory -> file.
    fs::rename(r.path().join("Foo.cs"), r.path().join("foo.cs")).unwrap();
    fs::remove_file(r.path().join("node")).unwrap();
    fs::create_dir_all(r.path().join("node")).unwrap();
    fs::write(r.path().join("node/inner"), "inner").unwrap();
    fs::remove_dir_all(r.path().join("tree/sub")).unwrap();
    fs::write(r.path().join("tree/sub"), "now a file").unwrap();
    snap.sync().unwrap();

    assert_eq!(fs::read_to_string(snap.dir.join("foo.cs")).unwrap(), "foo");
    assert!(snap.manifest.files.contains_key("foo.cs"));
    assert!(!snap.manifest.files.contains_key("Foo.cs"));
    assert_eq!(
        fs::read_to_string(snap.dir.join("node/inner")).unwrap(),
        "inner"
    );
    assert_eq!(
        fs::read_to_string(snap.dir.join("tree/sub")).unwrap(),
        "now a file"
    );
    // And the next sync has nothing to do.
    assert_eq!(snap.sync().unwrap().copied, 0);
}

/// F-29: a checked-out submodule's files are in the snapshot.
#[test]
fn snapshot_includes_checked_out_submodules() {
    let sub = tempfile::tempdir().unwrap();
    git(sub.path(), &["init", "-q"]);
    fs::write(sub.path().join("lib.txt"), "lib").unwrap();
    git(sub.path(), &["add", "lib.txt"]);
    git(
        sub.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "lib",
        ],
    );
    let r = repo();
    git(
        r.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &sub.path().to_string_lossy(),
            "vendor/lib",
        ],
    );
    let state = tempfile::tempdir().unwrap();
    let mut snap = Snapshot::open(r.path(), &state.path().join("lane")).unwrap();
    let files = snap.list().unwrap();
    assert!(
        files.contains(&"vendor/lib/lib.txt".to_string()),
        "{files:?}"
    );
    assert!(!files.contains(&"vendor/lib".to_string()), "{files:?}");
    snap.sync().unwrap();
    assert_eq!(
        fs::read_to_string(snap.dir.join("vendor/lib/lib.txt")).unwrap(),
        "lib"
    );
}

/// F-1: `build` and `output` are expanded like the rest of the stack file,
/// the same on every platform: `{repo}` is the snapshot, `${VAR}` comes from
/// the environment, `{{`/`}}` are braces.
#[test]
fn build_and_output_are_interpolated() {
    let r = repo();
    let state = tempfile::tempdir().unwrap();
    let layout = Layout::new(state.path());
    let mut snap = Snapshot::open(r.path(), &layout.lane("main")).unwrap();
    snap.sync().unwrap();
    let builder = Builder::new(layout.clone(), &Default::default()).unwrap();
    #[cfg(windows)]
    let cmd = "if not exist {repo}\\\\o2 mkdir {repo}\\\\o2 && echo ${DEVSTACK_TEST_UNSET:-fallback} > {repo}\\\\o2\\\\v.txt";
    #[cfg(not(windows))]
    let cmd = "mkdir -p {repo}/o2 && echo ${DEVSTACK_TEST_UNSET:-fallback} > {repo}/o2/v.txt";
    let svc = service(&format!(
        "build = \"{cmd}\"\noutput = \"{{repo}}/o2\"\nrun = \"x\"\n"
    ));
    let mut run = builder.start("main", &snap, "s", &svc).unwrap();
    let out = builder.wait(&mut run).unwrap();
    let Outcome::Published { dir, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(
        fs::read_to_string(dir.join("v.txt")).unwrap().trim(),
        "fallback"
    );
}

/// F-31: std turns long absolute paths into `\\?\` paths itself, so a
/// snapshot deeper than 260 characters copies on Windows as anywhere.
#[test]
fn deep_paths_copy() {
    let r = tempfile::tempdir().unwrap(); // no git: git itself needs core.longpaths
    let deep: String = (0..12)
        .map(|i| format!("directory-level-{i:02}/"))
        .collect();
    fs::create_dir_all(r.path().join(&deep)).unwrap();
    fs::write(r.path().join(format!("{deep}leaf.txt")), "leaf").unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut snap = Snapshot::open(r.path(), &state.path().join("lane")).unwrap();
    snap.sync().unwrap();
    let copy = snap.dir.join(format!("{deep}leaf.txt"));
    assert!(copy.to_string_lossy().len() > 260);
    assert_eq!(fs::read_to_string(copy).unwrap(), "leaf");
}
