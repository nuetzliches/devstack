import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
R='D:/Sources/devstack/.claude/worktrees/review-daemon/'
apply(R+'docs/dashboard-api.md',[
("""  main_served_by?: string;              // the lane that serves the main ports, if not the main tree
};""",
"""  main_served_by?: string;              // the lane that serves the main ports, if not the main tree
  watcher_error?: string;               // the file watcher could not be set up, or lost events: edits make no candidates (or every built service got one)
};"""),
("""  | { type: "usage" } & UsageReport                           // every 2 s, without history
);
```""",
"""  | { type: "usage" } & UsageReport                           // every 2 s, without history
);
```

A client that falls more than 1024 events behind (it stalled, or a burst
came) gets a fresh `snapshot` in place of what it missed: the state it holds
is replaced, never patched with a gap. A `snapshot` always replaces the whole
state, whenever it arrives; `seq` counts the daemon's events (a snapshot has
`0`), so a gap in it is visible."""),
("""| `POST /v1/down?infra=false` | `204` when every service stopped; with `infra=true` the containers too |""",
"""| `POST /v1/down?infra=false` | `204` when every service stopped (with `infra=true` the containers too); `409 {"error"}` naming what did not stop |"""),
("""| `GET /v1/runs/{id}/log` | text, a command run's output so far |""",
"""| `GET /v1/runs/{id}/log` | text, a command run's output so far (the last 20 000 lines) |
| `POST /v1/runs/{id}/cancel` | `202`; the run stops: its `exec` step's process tree is killed, its `run` step's job stopped, no further step starts; it ends `failed` with error `cancelled`. `409` when it is not running, `404` for no such run |"""),
("""| `POST /v1/services/{name}/start` \\| `stop` \\| `restart` \\| `promote` \\| `rollback` | `200 StackView`, or `409 {"error": "..."}` (e.g. compiler errors; the running instance stays). For a name that is no service but a group, `start`/`stop`/`restart` act on the group.""",
"""| `POST /v1/services/{name}/start` \\| `stop` \\| `restart` \\| `rebuild` \\| `promote` \\| `rollback` | `200 StackView`, or `409 {"error": "..."}` (e.g. compiler errors; the running instance stays). `rebuild` is `restart` that builds even when an artifact for the same inputs exists (`devstack restart --rebuild`; the build gets the id `<inputs>-<n>`). A `stop` ends a start in progress (a build, a health wait, a job) at once. For a name that is no service but a group, `start`/`stop`/`restart` act on the group."""),
("""| `GET /v1/services/{name}/logs?tail=200&grep=` | text, the last (matching, case-insensitive) lines of the current instance |""",
"""| `GET /v1/services/{name}/logs?tail=200&grep=` | text, the last (matching, case-insensitive) lines of the current instance; `tail` at most 20 000 |"""),
])
apply(R+'docs/stack-file.md',[
("""| `prepare = { run = "npm ci", inputs = ["web/package-lock.json"] }` | installs what a copy of the source lacks (see below) |""",
"""| `prepare = { run = "npm ci", inputs = ["web/package-lock.json"] }` | installs what a copy of the source lacks (see below) |
| `timeout = "10m"` | `kind = "job"` only: how long it may run before it counts as failed and is stopped; default 30 minutes. `devstack stop <job>` ends it at once |"""),
("""Each step has exactly one action: `exec`, `http` (`METHOD url`, with `headers`,
`body`, `timeout`), `start`, `stop`, `restart` or `run`. Commands run with""",
"""Each step has exactly one action: `exec`, `http` (`METHOD url`, with `headers`,
`body`, `timeout`), `start`, `stop`, `restart` or `run`. `timeout` bounds an
`exec` step too (default: none; `http` 30 s); a run can be cancelled
(`POST /v1/runs/{id}/cancel`). `exec` runs in the repository, `cwd` relative to
it; in a lane in the lane's copy, never in its worktree, and `{repo}` is that
copy. Commands run with"""),
])
print("ok")
