//! `GET /hub/summary` (ADR-0060 point 3): every registered stack at a glance,
//! and one state over all of them, for the tray. What needs attention is
//! what notifies (ADR-0045), read from the streams the hub follows for that
//! (`notify`): each stream leaves its [`Glance`] on the [`Board`] after every
//! event, and takes it away when it ends. The same items go to the
//! dashboard on `/hub/events` as an `attention` event, on every change
//! (ADR-0097).

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

use crate::{AppState, Phase, StackEntry};

/// How bad an [`Item`] is (ADR-0097): something failed, or something
/// waits for the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Level {
    #[default]
    Error,
    Warn,
}

/// Something that needs attention.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Item {
    /// The lane it is in; `None`: the main working tree.
    pub lane: Option<String>,
    /// The service it is about; `None`: the stack's start.
    pub service: Option<String>,
    /// One short line: `api: build held for promote`.
    pub text: String,
    /// The first line of why, or empty.
    pub detail: String,
    pub level: Level,
}

/// What one stream (a stack's main tree, or one of its lanes) shows now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Glance {
    /// Services running, healthy or done (a job that ended with 0).
    pub ready: usize,
    pub total: usize,
    /// A service builds or starts.
    pub starting: bool,
    pub attention: Vec<Item>,
}

/// Each followed stream's last [`Glance`], by stack id and lane.
pub(crate) struct Board {
    streams: Mutex<HashMap<String, BTreeMap<Option<String>, Glance>>>,
    /// Moves when a glance changed: the dashboard's `attention` event.
    changed: watch::Sender<u64>,
}

impl Default for Board {
    fn default() -> Self {
        Self {
            streams: Mutex::default(),
            changed: watch::Sender::new(0),
        }
    }
}

impl Board {
    pub fn set(&self, stack: &str, lane: Option<&str>, g: Glance) {
        let before = self
            .streams
            .lock()
            .unwrap()
            .entry(stack.to_string())
            .or_default()
            .insert(lane.map(str::to_string), g.clone());
        if before.as_ref() != Some(&g) {
            self.changed.send_modify(|n| *n += 1);
        }
    }

    fn remove(&self, stack: &str, lane: Option<&str>) {
        let mut streams = self.streams.lock().unwrap();
        if let Some(by_lane) = streams.get_mut(stack) {
            by_lane.remove(&lane.map(str::to_string));
            if by_lane.is_empty() {
                streams.remove(stack);
            }
        }
        drop(streams);
        self.changed.send_modify(|n| *n += 1);
    }

    /// Moves on every change of a glance.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Takes the stream's glance off the board when dropped: when its
    /// stream ends, or its task is aborted (the stack or lane stopped).
    pub fn hold(self: &Arc<Self>, stack: &str, lane: Option<&str>) -> Held {
        Held {
            board: self.clone(),
            stack: stack.to_string(),
            lane: lane.map(str::to_string),
        }
    }

    fn of(&self, stack: &str) -> BTreeMap<Option<String>, Glance> {
        self.streams
            .lock()
            .unwrap()
            .get(stack)
            .cloned()
            .unwrap_or_default()
    }
}

pub(crate) struct Held {
    board: Arc<Board>,
    stack: String,
    lane: Option<String>,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.board.remove(&self.stack, self.lane.as_deref());
    }
}

/// One stack in the summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StackSummary {
    pub id: String,
    pub name: String,
    /// The main working tree (`POST /hub/open` opens it).
    pub repo: std::path::PathBuf,
    pub running: bool,
    /// `stopped`, `starting`, `stopping`, `ok` or `attention`.
    pub state: &'static str,
    /// The main tree's services ready, of all.
    pub ready: usize,
    pub total: usize,
    /// Lanes whose stream the hub follows: started, here or on a node.
    pub lanes: usize,
    pub attention: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Summary {
    /// `attention`, `starting` (a stack starts or stops), `ok` (a stack
    /// runs) or `idle`: the worst of the stacks.
    pub state: &'static str,
    pub version: &'static str,
    pub stacks: Vec<StackSummary>,
}

fn stack(
    e: &StackEntry,
    phase: Option<Phase>,
    streams: &BTreeMap<Option<String>, Glance>,
) -> StackSummary {
    let main = streams.get(&None).cloned().unwrap_or_default();
    let mut attention = Vec::new();
    if let Some(err) = &e.start_error {
        attention.push(Item {
            lane: None,
            service: None,
            text: "the start failed".into(),
            detail: err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string(),
            level: Level::Error,
        });
    }
    let mut starting = false;
    for (lane, g) in streams {
        if e.running {
            starting |= g.starting;
            attention.extend(g.attention.iter().cloned().map(|mut i| {
                i.lane = lane.clone();
                i
            }));
        }
    }
    let state = match phase {
        Some(Phase::Starting) => "starting",
        Some(Phase::Stopping) => "stopping",
        None if !attention.is_empty() => "attention",
        None if !e.running => "stopped",
        None if starting => "starting",
        None => "ok",
    };
    StackSummary {
        id: e.id.clone(),
        name: e.name.clone(),
        repo: e.repo.clone(),
        running: e.running,
        state,
        ready: main.ready,
        total: main.total,
        lanes: streams.keys().filter(|l| l.is_some()).count(),
        attention,
    }
}

fn overall(stacks: &[StackSummary]) -> &'static str {
    let any = |s: &[&str]| stacks.iter().any(|x| s.contains(&x.state));
    if any(&["attention"]) {
        "attention"
    } else if any(&["starting", "stopping"]) {
        "starting"
    } else if stacks.iter().any(|x| x.running) {
        "ok"
    } else {
        "idle"
    }
}

pub(crate) fn summary(s: &AppState) -> Summary {
    let entries = s.stacks.borrow().clone();
    let phases = s.phases.lock().unwrap().clone();
    let stacks: Vec<StackSummary> = entries
        .iter()
        .map(|e| stack(e, phases.get(&e.id).copied(), &s.board.of(&e.id)))
        .collect();
    Summary {
        state: overall(&stacks),
        version: crate::VERSION,
        stacks,
    }
}

/// `GET /hub/summary`.
pub(crate) async fn get(State(s): State<AppState>) -> Response {
    Json(summary(&s)).into_response()
}

/// One item of the dashboard's `attention` event: an [`Item`] and its stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Attention {
    pub stack: String,
    #[serde(flatten)]
    pub item: Item,
}

/// What needs attention in every stack, for `/hub/events` (ADR-0097).
pub(crate) fn attention(s: &AppState) -> Vec<Attention> {
    summary(s)
        .stacks
        .into_iter()
        .flat_map(|st| {
            st.attention.into_iter().map(move |item| Attention {
                stack: st.id.clone(),
                item,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, running: bool) -> StackEntry {
        StackEntry {
            id: id.into(),
            name: format!("stack {id}"),
            file: Default::default(),
            repo: Default::default(),
            running,
            daemon: None,
            last_seen: None,
            last_commit: None,
            start_error: None,
            shift: 0,
            no_file: false,
        }
    }

    fn held(service: &str) -> Item {
        Item {
            lane: None,
            service: Some(service.into()),
            text: format!("{service}: build held for promote"),
            detail: String::new(),
            level: Level::Warn,
        }
    }

    #[test]
    fn a_lanes_attention_is_the_stacks_and_names_the_lane() {
        let mut streams = BTreeMap::new();
        streams.insert(
            None,
            Glance {
                ready: 2,
                total: 3,
                ..Default::default()
            },
        );
        streams.insert(
            Some("feat-x".to_string()),
            Glance {
                attention: vec![held("api")],
                ..Default::default()
            },
        );
        let s = stack(&entry("ab12", true), None, &streams);
        assert_eq!(s.state, "attention");
        assert_eq!((s.ready, s.total, s.lanes), (2, 3, 1));
        assert_eq!(s.attention[0].lane.as_deref(), Some("feat-x"));
        assert_eq!(overall(&[s]), "attention");
    }

    #[test]
    fn a_stopped_stack_shows_its_failed_start_and_nothing_stale() {
        let mut e = entry("ab12", false);
        e.start_error = Some("Error: port 5432 is taken\n\n".into());
        // A stream not taken off the board yet says nothing for a stopped stack.
        let streams = BTreeMap::from([(
            None,
            Glance {
                attention: vec![held("api")],
                ..Default::default()
            },
        )]);
        let s = stack(&e, None, &streams);
        assert_eq!(s.state, "attention");
        assert_eq!(s.attention.len(), 1);
        assert_eq!(s.attention[0].detail, "Error: port 5432 is taken");
        let s = stack(&entry("cd34", false), None, &BTreeMap::new());
        assert_eq!(s.state, "stopped");
        assert_eq!(overall(&[s]), "idle");
    }

    #[test]
    fn a_start_or_stop_under_way_wins_and_the_worst_stack_is_the_whole() {
        let ok = stack(&entry("ab12", true), None, &BTreeMap::new());
        assert_eq!(ok.state, "ok");
        let starting = stack(
            &entry("cd34", false),
            Some(Phase::Starting),
            &BTreeMap::new(),
        );
        assert_eq!(starting.state, "starting");
        let stopping = stack(
            &entry("ef56", true),
            Some(Phase::Stopping),
            &BTreeMap::new(),
        );
        assert_eq!(stopping.state, "stopping");
        assert_eq!(overall(std::slice::from_ref(&ok)), "ok");
        assert_eq!(overall(&[ok, stopping]), "starting");
    }

    #[test]
    fn a_held_glance_goes_with_its_stream() {
        let board = Arc::new(Board::default());
        {
            let _held = board.hold("ab12", Some("feat-x"));
            board.set("ab12", Some("feat-x"), Glance::default());
            board.set("ab12", None, Glance::default());
            assert_eq!(board.of("ab12").len(), 2);
        }
        assert_eq!(board.of("ab12").len(), 1);
    }

    #[test]
    fn the_board_says_when_a_glance_changed_and_only_then() {
        let board = Board::default();
        let mut rx = board.subscribe();
        rx.mark_unchanged();
        board.set("ab12", None, Glance::default());
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        board.set("ab12", None, Glance::default());
        assert!(!rx.has_changed().unwrap());
        board.set(
            "ab12",
            None,
            Glance {
                attention: vec![held("api")],
                ..Default::default()
            },
        );
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn an_item_says_its_level() {
        let v = serde_json::to_value(Attention {
            stack: "ab12".into(),
            item: held("api"),
        })
        .unwrap();
        assert_eq!(v["stack"], "ab12");
        assert_eq!(v["level"], "warn");
        assert_eq!(v["text"], "api: build held for promote");
    }
}
