//! A pool's skill sets (ADR-0074): only an admin names a set; a member reads
//! and downloads only the sets given to it, each download audited; its
//! stream tells it when its sets changed; a signed refresh fetches again.

use devstack_node::coordinator;
use devstack_node::events::{self, Ended, Heard};
use devstack_node::member::{Local, MemberError};
use devstack_node::skills::{SkillFolder, SkillSet, Source};
use devstack_node::store::{Role, SHELL, Store};
use std::path::Path;
use std::time::{Duration, Instant};

fn coordinator(dir: &Path) -> (String, String) {
    let fp = coordinator::init(dir, &["127.0.0.1".into()]).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let url = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        rt.block_on(coordinator::serve(&dir, listener, std::future::pending()))
            .unwrap();
    });
    (url, fp)
}

fn member(coord: &Path, url: &str, fp: &str, dir: &Path, name: &str, role: Role) -> Local {
    let l = Local::new(dir);
    let m = l.join(url, fp, name).unwrap();
    Store::new(coord)
        .approve(&m.id, &coordinator::ca(coord).unwrap(), Some(role), SHELL)
        .unwrap();
    l.refresh().unwrap();
    l
}

fn skill(coord: &Path, name: &str, text: &str) {
    let d = coord.join("skill-sources").join("core").join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {text}\n---\nDo it."),
    )
    .unwrap();
}

fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < end, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_set_goes_to_whom_it_is_given_and_its_change_is_told() {
    let c = tempfile::tempdir().unwrap();
    let (url, fp) = coordinator(c.path());
    skill(c.path(), "work-issue", "Work on an issue");
    let ad = tempfile::tempdir().unwrap();
    let admin = member(c.path(), &url, &fp, ad.path(), "lead-pc", Role::Admin);
    let md = tempfile::tempdir().unwrap();
    let dev = member(c.path(), &url, &fp, md.path(), "dev-pc", Role::Member);
    let od = tempfile::tempdir().unwrap();
    let other = member(c.path(), &url, &fp, od.path(), "guest-pc", Role::Member);
    let dev_id = dev.membership().unwrap().unwrap().id;
    admin.admin_tags(&dev_id, &["team".into()]).unwrap();

    let set = SkillSet {
        name: "core".into(),
        source: Source::Folder {
            path: "core".into(),
        },
        to: vec!["team".into()],
        repos: Vec::new(),
        folders: Vec::new(),
    };
    assert!(matches!(
        dev.admin_skill_put(&set),
        Err(MemberError::Status(403, _))
    ));
    admin.admin_skill_put(&set).unwrap();

    let sets = until("the set is fetched", || {
        let s = dev.skills().unwrap();
        (s.len() == 1 && !s[0].skills.is_empty()).then_some(s)
    });
    assert_eq!(sets[0].skills[0].name, "work-issue");
    assert_eq!(sets[0].source, "folder:core");
    assert!(other.skills().unwrap().is_empty(), "not given to guest-pc");

    let bytes = dev.skill_archive("core", "work-issue").unwrap();
    let out = md.path().join("unpacked");
    devstack_node::skills::unpack(&bytes, &out).unwrap();
    assert!(out.join("SKILL.md").exists());
    assert!(matches!(
        other.skill_archive("core", "work-issue"),
        Err(MemberError::Status(404, _))
    ));
    let audit = admin.admin_audit().unwrap();
    assert!(
        audit.iter().any(|e| e.action == "skill"
            && e.detail
                .as_deref()
                .unwrap_or("")
                .starts_with("core/work-issue")),
        "the download is in the audit log"
    );

    // The member following the pool hears its sets change.
    let (tx, rx) = std::sync::mpsc::channel();
    let follower = Local::new(md.path());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let t = tx.clone();
        let ended: Ended = rt.block_on(events::follow(follower, move |h| {
            if let Heard::Changed(v) = h {
                let _ = t.send(v);
            }
        }));
        drop(ended);
    });
    let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(first.contains_key(events::SKILLS));
    skill(c.path(), "push-tag", "Push a tag");
    admin.admin_skills_refresh(None).unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let v = rx
            .recv_timeout(end.saturating_duration_since(Instant::now()))
            .expect("the change of the set is told");
        if v.contains_key(events::SKILLS) {
            break;
        }
    }
    assert_eq!(dev.skills().unwrap()[0].skills.len(), 2);

    admin.admin_skill_remove("core").unwrap();
    assert!(dev.skills().unwrap().is_empty());
}

#[test]
fn each_folder_of_a_set_goes_to_its_own_members() {
    let c = tempfile::tempdir().unwrap();
    let (url, fp) = coordinator(c.path());
    // `core/work-issue` for everyone, `core/projects/shop/shop-page` for
    // the tag `shop` only, bound to its repository by the folder.
    skill(c.path(), "work-issue", "Work on an issue");
    let d = c.path().join("skill-sources/core/projects/shop/shop-page");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("SKILL.md"),
        "---\nname: shop-page\ndescription: A page of the shop\n---\n",
    )
    .unwrap();
    let ad = tempfile::tempdir().unwrap();
    let admin = member(c.path(), &url, &fp, ad.path(), "lead-pc", Role::Admin);
    let md = tempfile::tempdir().unwrap();
    let dev = member(c.path(), &url, &fp, md.path(), "dev-pc", Role::Member);
    let od = tempfile::tempdir().unwrap();
    let other = member(c.path(), &url, &fp, od.path(), "guest-pc", Role::Member);
    let dev_id = dev.membership().unwrap().unwrap().id;
    admin.admin_tags(&dev_id, &["shop".into()]).unwrap();

    let set = SkillSet {
        name: "core".into(),
        source: Source::Folder {
            path: "core".into(),
        },
        to: Vec::new(),
        repos: Vec::new(),
        folders: vec![
            SkillFolder {
                path: String::new(),
                to: vec!["*".into()],
                repos: Vec::new(),
            },
            SkillFolder {
                path: "projects/shop".into(),
                to: vec!["shop".into()],
                repos: vec!["acme/shop".into()],
            },
        ],
    };
    admin.admin_skill_put(&set).unwrap();
    let names = |l: &Local| -> Vec<String> {
        l.skills()
            .unwrap()
            .iter()
            .flat_map(|m| m.skills.iter().map(|k| k.name.clone()))
            .collect()
    };
    until("the set is fetched", || {
        (names(&dev).len() == 2).then_some(())
    });
    let mine = dev.skills().unwrap();
    let shop = mine[0]
        .skills
        .iter()
        .find(|k| k.name == "shop-page")
        .unwrap();
    assert_eq!(shop.repos, ["acme/shop"]);
    assert_eq!(shop.folder.as_deref(), Some("projects/shop"));
    assert_eq!(names(&other), ["work-issue"]);
    assert!(other.skill_archive("core", "work-issue").is_ok());
    assert!(
        matches!(
            other.skill_archive("core", "shop-page"),
            Err(MemberError::Status(404, _))
        ),
        "a folder not given to it is not downloaded either"
    );
    assert!(dev.skill_archive("core", "shop-page").is_ok());
}

#[test]
fn a_refresh_without_a_signature_is_refused() {
    let c = tempfile::tempdir().unwrap();
    let (url, _) = coordinator(c.path());
    let agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .disable_verification(true)
                .build(),
        )
        .http_status_as_error(false)
        .build()
        .new_agent();
    // Without the secret on the coordinator: there is no such route.
    let r = agent
        .post(&format!("{url}/v1/skills/refresh"))
        .send("{}")
        .unwrap();
    assert_eq!(r.status(), 404);
}
