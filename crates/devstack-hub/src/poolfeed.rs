//! The pools this machine is an approved member of, followed over their
//! coordinators' event streams (ADR-0073): the hub reads a part of a pool
//! only when its stream says it changed, never on a timer.
//!
//! - `leases` or `nodes`: the guest lanes on this machine (`guests`)
//! - `upstreams`: the pool's MCP upstreams (ADR-0069 option B)
//! - `skills`: the skill sets the pool gives this machine (ADR-0074)
//! - every change: the dashboard hears the pools' versions on `/hub/events`
//!   (`pools`) and reads what it shows of a pool again when they differ
//!
//! A coordinator without a stream (a release before it) is read once when
//! the hub starts following it and when the dashboard opens the pool area;
//! the dashboard says the pool is not live. Which pools are followed is
//! looked at again when the hub starts, when this machine joins a pool and
//! when the dashboard reads the pools.

use devstack_node::events::{self, Ended, Heard, Versions};
use devstack_node::member::Local;
use devstack_node::store::State;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::watch;

/// How a pool is followed, as the dashboard shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Follow {
    /// Connecting, or connecting again after a loss.
    Connecting,
    /// The stream is open: what the dashboard shows is current.
    Live,
    /// The stream broke; connecting again (`error` says why).
    Lost,
    /// The coordinator has no stream: read when the pool area opens.
    Old,
}

/// One pool, as `/hub/events` tells it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pool {
    pub live: Follow,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The version of each part of the pool, as the stream told last.
    pub versions: Versions,
}

struct Feed {
    tx: watch::Sender<BTreeMap<String, Pool>>,
    /// The followers, by pool, with the membership's directory.
    followers: Mutex<HashMap<String, (PathBuf, tokio::task::JoinHandle<()>)>>,
    upstreams: Arc<crate::upstreams::Upstreams>,
    skills: Arc<crate::poolskills::PoolSkills>,
    rt: tokio::runtime::Handle,
}

static FEED: OnceLock<Feed> = OnceLock::new();

/// Start following this machine's pools (once per hub).
pub fn start(
    upstreams: Arc<crate::upstreams::Upstreams>,
    skills: Arc<crate::poolskills::PoolSkills>,
) {
    let feed = Feed {
        tx: watch::channel(BTreeMap::new()).0,
        followers: Mutex::new(HashMap::new()),
        upstreams,
        skills,
        rt: tokio::runtime::Handle::current(),
    };
    if FEED.set(feed).is_ok() {
        sync();
    }
}

/// The pools now, and each change.
pub fn subscribe() -> watch::Receiver<BTreeMap<String, Pool>> {
    match FEED.get() {
        Some(f) => f.tx.subscribe(),
        None => watch::channel(BTreeMap::new()).1,
    }
}

/// How `pool` is followed; `None` when it is not (no approved member, or
/// a hub that does not follow pools, as in tests).
pub fn live(pool: &str) -> Option<Follow> {
    FEED.get()?.tx.borrow().get(pool).map(|p| p.live)
}

pub(crate) fn state() -> PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

/// Follow every pool this machine is an approved member of, and stop
/// following the ones it is not (any more).
pub fn sync() {
    let Some(feed) = FEED.get() else { return };
    let approved: BTreeMap<String, PathBuf> = devstack_node::member::pools(&state())
        .into_iter()
        .filter(|(_, dir)| {
            Local::new(dir)
                .membership()
                .ok()
                .flatten()
                .is_some_and(|m| m.state == State::Approved)
        })
        .collect();
    let mut followers = feed.followers.lock().unwrap();
    let gone: Vec<String> = followers
        .iter()
        .filter(|(p, (dir, _))| approved.get(*p) != Some(dir))
        .map(|(p, _)| p.clone())
        .collect();
    for p in gone {
        if let Some((_, task)) = followers.remove(&p) {
            task.abort();
        }
        left(feed, &p);
    }
    for (pool, dir) in approved {
        if followers.contains_key(&pool) {
            continue;
        }
        let task = feed.rt.spawn(follow(pool.clone(), dir.clone()));
        followers.insert(pool, (dir, task));
    }
}

/// `pool` is followed no more: what came from it goes.
fn left(feed: &Feed, pool: &str) {
    feed.tx.send_if_modified(|m| m.remove(pool).is_some());
    let ups = feed.upstreams.clone();
    let skills = feed.skills.clone();
    let pool = pool.to_string();
    feed.rt.spawn_blocking(move || {
        ups.set_pool(&pool, None);
        skills.set_pool(&pool, None);
        crate::guests::seen(&pool, Some(Vec::new()));
    });
}

fn set(pool: &str, change: impl FnOnce(&mut Pool)) {
    let Some(feed) = FEED.get() else { return };
    feed.tx.send_if_modified(|m| {
        let p = m.entry(pool.to_string()).or_insert_with(|| Pool {
            live: Follow::Connecting,
            error: None,
            versions: Versions::new(),
        });
        let before = p.clone();
        change(p);
        *p != before
    });
}

/// Read what `topics` of `pool` this hub keeps: its guests, its upstreams.
fn read(pool: &str, dir: &std::path::Path, topics: &[&str]) {
    let Some(feed) = FEED.get() else { return };
    let guests = topics.contains(&events::LEASES) || topics.contains(&events::NODES);
    let upstreams = topics.contains(&events::UPSTREAMS);
    let skill_sets = topics.contains(&events::SKILLS);
    if !guests && !upstreams && !skill_sets {
        return;
    }
    let (pool, dir, ups) = (pool.to_string(), dir.to_path_buf(), feed.upstreams.clone());
    let skills = feed.skills.clone();
    feed.rt.spawn_blocking(move || {
        if guests {
            crate::guests::seen(&pool, crate::share::guests(&pool));
        }
        if upstreams {
            // A coordinator that cannot be asked now: the list it named
            // last stays, and the next connect brings the version again.
            if let Ok(list) = Local::new(&dir).upstreams() {
                ups.set_pool(&pool, Some(list));
            }
        }
        if skill_sets && let Ok(sets) = Local::new(&dir).skills() {
            skills.set_pool(&pool, Some(sets));
        }
    });
}

/// Read every part of `pool` once: its coordinator has no stream, and the
/// dashboard opened the pool area (or the hub started following it).
pub fn read_once(pool: &str) {
    let Some(feed) = FEED.get() else { return };
    let dir = feed
        .followers
        .lock()
        .unwrap()
        .get(pool)
        .map(|(d, _)| d.clone());
    if let Some(dir) = dir {
        read(pool, &dir, &events::TOPICS);
    }
}

async fn follow(pool: String, dir: PathBuf) {
    set(&pool, |p| p.live = Follow::Connecting);
    let (p, d) = (pool.clone(), dir.clone());
    let ended = events::follow(Local::new(&dir), move |h| match h {
        Heard::Connected => set(&p, |x| {
            x.live = Follow::Live;
            x.error = None;
        }),
        Heard::Changed(v) => {
            let topics: Vec<&str> = v.keys().map(String::as_str).collect();
            read(&p, &d, &topics);
            set(&p, |x| x.versions.extend(v.clone()));
        }
        Heard::Lost(e) => set(&p, |x| {
            x.live = Follow::Lost;
            x.error = Some(e);
        }),
    })
    .await;
    match ended {
        Ended::Unsupported => {
            set(&pool, |x| {
                x.live = Follow::Old;
                x.error = Some(
                    "the pool's coordinator runs a devstack before 0.29.0: the pool area is read when it opens, not live"
                        .into(),
                );
            });
            read(&pool, &dir, &events::TOPICS);
        }
        // Removed from the pool, or the membership gone here: the next
        // look at the pools finds out which, and what came from it goes.
        Ended::NotMember(_) | Ended::NoMembership(_) => {
            if let Some(feed) = FEED.get() {
                feed.followers.lock().unwrap().remove(&pool);
                left(feed, &pool);
            }
        }
    }
}
