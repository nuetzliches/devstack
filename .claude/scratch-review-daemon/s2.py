import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
P='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/state.rs'
apply(P,[
("""    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_served_by: Option<String>,
}""","""    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_served_by: Option<String>,
    /// Why the file watcher does not work (it could not be set up, or the
    /// OS dropped its events): edits then make no candidates until it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watcher_error: Option<String>,
}"""),
("""    /// Process trees to clean up after a crash (Linux; ADR-0016).
    #[serde(default)]
    pub records: Vec<devstack_proc::Record>,
}""","""    /// Process trees to clean up after a crash (Linux; ADR-0016).
    #[serde(default)]
    pub records: Vec<devstack_proc::Record>,
    /// Service -> the candidate a promotion gate holds (ADR-0013), so the
    /// gate outlives a daemon restart.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub held: BTreeMap<String, HeldRecord>,
}

/// A held candidate as `state.json` keeps it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeldRecord {
    /// Empty while the newest build failed (the gate stays).
    pub build_id: String,
    pub reason: String,
}"""),
])
print("ok")
