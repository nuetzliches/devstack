//! Cross-reference checks that the deserialiser cannot do. All problems are
//! collected and reported together, each with the path of the offending key.

use crate::interpolate::{self, Placeholder, Segment};
use crate::model::{Kind, Member, SCHEMA_VERSION, Stack, StepAction};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Dotted path of the key, e.g. `services.api.after[0]`.
    pub path: String,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Where an interpolated string is used: which placeholders have a value there.
#[derive(Clone, Copy)]
enum Scope {
    /// What starts a service: `run`, `env`, worker switches, `ensure_dirs`.
    Service { port: bool, artifact: bool },
    /// `build` and `output`: expanded by the builder in the snapshot, for an
    /// artifact every lane shares, so `{repo}` and `${VAR}` only.
    Build,
    /// A command's steps.
    Command,
}

/// Names Windows reserves for devices in every directory: a service called
/// `con` could not have a log directory there.
const WINDOWS_DEVICES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Largest `[lanes] port_step`. Lane numbers grow from 1 and every lane's
/// ports must stay below 65536; the daemon refuses a lane whose ports do not.
pub const MAX_PORT_STEP: u16 = 10_000;

/// A path relative to the repository that stays inside it, judged the same on
/// every platform: no root (`/x`, `\x`), no drive or scheme (`C:`), no `..`.
pub fn inside_repo(p: &str) -> bool {
    !p.trim().is_empty()
        && !p.starts_with(['/', '\\'])
        && !p.contains(':')
        && !p.split(['/', '\\']).any(|c| c == "..")
}

/// The message for a stack file of another schema version.
pub fn unsupported_version(v: impl fmt::Display) -> String {
    format!("unsupported version {v} (this devstack reads version {SCHEMA_VERSION})")
}

struct Check<'a> {
    stack: &'a Stack,
    issues: Vec<Issue>,
}

impl Check<'_> {
    fn err(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(Issue {
            path: path.into(),
            message: message.into(),
        });
    }

    fn name(&mut self, path: &str, name: &str) {
        let ok = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !name.starts_with('-');
        if !ok {
            self.err(
                path,
                format!("`{name}`: names use lowercase letters, digits and `-`"),
            );
        } else if WINDOWS_DEVICES.contains(&name) {
            self.err(
                path,
                format!("`{name}` is a device name on Windows and cannot name a directory"),
            );
        }
    }

    /// A name Compose and Docker take, starting with a letter or digit (a
    /// leading `-` would be read as a flag): a project in lowercase letters,
    /// digits, `_` and `-`; a service in letters, digits, `_` and `-` (no `.`:
    /// `compose.<group>.<service>` splits on it); a network may also have `.`.
    fn compose_ident(&mut self, path: &str, what: &str, s: &str, lowercase: bool, dot: bool) {
        let letter = |c: char| c.is_ascii_lowercase() || (!lowercase && c.is_ascii_uppercase());
        let first_ok = s
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || letter(c));
        let rest_ok = s
            .chars()
            .all(|c| c.is_ascii_digit() || letter(c) || c == '_' || c == '-' || (dot && c == '.'));
        if !(first_ok && rest_ok) {
            let chars = match (lowercase, dot) {
                (true, _) => "lowercase letters, digits, `_` and `-`",
                (false, true) => "letters, digits, `_`, `-` and `.`",
                (false, false) => "letters, digits, `_` and `-`",
            };
            self.err(
                path,
                format!("`{s}`: a {what} name uses {chars} and starts with a letter or digit"),
            );
        }
    }

    /// A path relative to the repository that must stay inside it.
    fn repo_path(&mut self, path: &str, p: &str) {
        if !inside_repo(p) {
            self.err(
                path,
                format!("`{p}` must be a path inside the repository, relative to it"),
            );
        }
    }

    /// A string devstack uses as written: a placeholder or `${VAR}` in it
    /// would reach the shell or the file system verbatim.
    fn literal(&mut self, path: &str, s: &str) {
        let expands = interpolate::parse(s)
            .is_ok_and(|segs| segs.iter().any(|x| !matches!(x, Segment::Literal(_))));
        if expands {
            self.err(
                path,
                format!(
                    "`{s}`: placeholders and `${{VAR}}` are not expanded here; write the value itself"
                ),
            );
        }
    }

    /// Glob patterns, compiled as the watcher compiles them.
    fn globs(&mut self, path: &str, patterns: &[String]) {
        for g in patterns {
            if let Err(e) = globset::GlobBuilder::new(g).literal_separator(true).build() {
                self.err(path, format!("`{g}`: {e}"));
            }
        }
    }

    /// Syntax and references of every `{…}` placeholder in a string, and
    /// whether each has a value where the string is used.
    fn text(&mut self, path: &str, s: &str, scope: Scope) {
        let segs = match interpolate::parse(s) {
            Ok(segs) => segs,
            Err(e) => return self.err(path, e.to_string()),
        };
        for p in interpolate::placeholders(&segs) {
            let unavailable = match (scope, p) {
                (Scope::Build, Placeholder::Repo) => None,
                (Scope::Build, _) => Some(
                    "is not known to a build: `build` and `output` take `{repo}` (the snapshot) and `${VAR}`, since every lane shares the artifact",
                ),
                (Scope::Service { port: false, .. }, Placeholder::Port) => {
                    Some("the service has no `port`")
                }
                (
                    Scope::Service {
                        artifact: false, ..
                    },
                    Placeholder::Artifact,
                ) => Some("only a service started from a build has an artifact"),
                (
                    Scope::Command,
                    Placeholder::Port | Placeholder::Artifact | Placeholder::IsMain,
                ) => Some("is not available in a command"),
                _ => None,
            };
            if let Some(why) = unavailable {
                self.err(path, format!("`{p}`: {why}"));
                continue;
            }
            match p {
                Placeholder::ServiceUrl(name) | Placeholder::ServicePort(name) => {
                    match self.stack.services.get(name) {
                        None => self.err(path, format!("`{p}`: no service `{name}`")),
                        Some(svc) if svc.port.is_none() => {
                            self.err(path, format!("`{p}`: service `{name}` has no port"))
                        }
                        _ => {}
                    }
                }
                Placeholder::ComposePort { group, service, .. } => {
                    self.compose_service(path, &format!("{p}"), group, service)
                }
                Placeholder::ComposeProject(group) if !self.stack.compose.contains_key(group) => {
                    self.err(path, format!("`{p}`: no compose group `{group}`"))
                }
                _ => {}
            }
        }
    }

    fn compose_service(&mut self, path: &str, what: &str, group: &str, service: &str) {
        match self.stack.compose.get(group) {
            None => self.err(path, format!("`{what}`: no compose group `{group}`")),
            Some(g) if !g.services.iter().any(|s| s == service) => {
                self.err(path, format!("`{what}`: `{service}` is not started by compose group `{group}` (add it to `services`)"))
            }
            _ => {}
        }
    }

    fn service_ref(&mut self, path: &str, name: &str) -> bool {
        if self.stack.services.contains_key(name) {
            true
        } else {
            self.err(path, format!("no service `{name}`"));
            false
        }
    }
}

pub fn validate(stack: &Stack) -> Vec<Issue> {
    let mut c = Check {
        stack,
        issues: Vec::new(),
    };
    // Each profile is checked as the stack it makes (ADR-0041), once: not
    // again on a stack one was applied to.
    for name in stack.profiles.keys().filter(|_| stack.profile.is_none()) {
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || name.is_empty()
        {
            c.err(
                format!("profiles.{name}"),
                "a profile's name uses letters, digits, `-` and `_`",
            );
            continue;
        }
        // The profile's `env` reaches every command's `exec` steps too, where
        // `{port}`, `{artifact}` and `{is_main}` have no value; in services
        // it is checked below, with each service's.
        for (k, v) in &stack.profiles[name].env {
            c.text(&format!("profiles.{name}.env.{k}"), v, Scope::Command);
        }
        match stack.apply_profile(name) {
            Ok(s) => {
                for i in validate(&s) {
                    c.err(format!("profiles.{name} ({})", i.path), i.message);
                }
            }
            Err(issues) => c.issues.extend(issues),
        }
    }

    if stack.version != SCHEMA_VERSION {
        c.err("version", unsupported_version(stack.version));
    }
    if stack.stack.name.trim().is_empty() {
        c.err("stack.name", "must not be empty");
    }
    if let Some(r) = &stack.stack.requires
        && crate::version_triple(r).is_none()
    {
        c.err(
            "stack.requires",
            format!("`{r}` is not a version like `0.1.0`"),
        );
    }
    for (i, f) in stack.stack.env_files.iter().enumerate() {
        c.repo_path(&format!("stack.env_files[{i}]"), f);
    }

    check_need(&mut c, "lanes.need", &stack.lanes.need);
    for (name, p) in stack.profiles.iter().filter(|_| stack.profile.is_none()) {
        check_need(&mut c, &format!("profiles.{name}.need"), &p.need);
    }

    let step = stack.lanes.port_step;
    if step == 0 {
        c.err(
            "lanes.port_step",
            "must be at least 1: every lane moves its ports by it",
        );
    } else if step > MAX_PORT_STEP {
        c.err(
            "lanes.port_step",
            format!("must be at most {MAX_PORT_STEP}: lanes move their ports by multiples of it"),
        );
    }

    if let Some(img) = &stack.lanes.image
        && img.trim().is_empty()
    {
        c.err(
            "lanes.image",
            "must not be empty: an image name, e.g. `registry/shop-lane:1`",
        );
    }
    if let Some(df) = &stack.lanes.dockerfile
        && !inside_repo(&df.to_string_lossy())
    {
        c.err(
            "lanes.dockerfile",
            "must be a path inside the repository, relative to the stack file",
        );
    }

    for g in &stack.lanes.sync {
        if !inside_repo(g) {
            c.err(
                "lanes.sync",
                format!("`{g}` must be a path inside the repository, relative to it"),
            );
        } else if let Err(e) = globset::Glob::new(g) {
            c.err("lanes.sync", format!("`{g}`: {e}"));
        }
    }

    if let Some(dirs) = &stack.agents.skills {
        if dirs.is_empty() {
            c.err(
                "agents.skills",
                "must name at least one folder; leave it out for `.claude/skills`",
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for d in dirs {
            if !inside_repo(d) {
                c.err(
                    "agents.skills",
                    format!(
                        "`{d}` must be a folder inside the repository, relative to the stack file"
                    ),
                );
            } else if !seen.insert(d.replace('\\', "/").trim_end_matches('/').to_string()) {
                c.err("agents.skills", format!("`{d}` is named twice"));
            }
        }
    }

    // ---------------------------------------------------------- compose groups
    let mut projects = BTreeMap::new();
    for (g, group) in &stack.compose {
        let p = format!("compose.{g}");
        c.name(&p, g);
        if group.project.trim().is_empty() {
            c.err(format!("{p}.project"), "must not be empty");
        } else {
            c.compose_ident(
                &format!("{p}.project"),
                "Compose project",
                &group.project,
                true,
                false,
            );
            if let Some(other) = projects.insert(group.project.clone(), g) {
                c.err(
                    format!("{p}.project"),
                    format!(
                        "project `{}` is also used by group `{other}`",
                        group.project
                    ),
                );
            }
        }
        if group.files.is_empty() {
            c.err(format!("{p}.files"), "needs at least one Compose file");
        }
        for (i, f) in group.files.iter().enumerate() {
            c.repo_path(&format!("{p}.files[{i}]"), f);
        }
        for (i, f) in group.env_files.iter().enumerate() {
            c.repo_path(&format!("{p}.env_files[{i}]"), f);
        }
        if group.services.is_empty() {
            c.err(
                format!("{p}.services"),
                "needs at least one service to start",
            );
        }
        for (i, svc) in group.services.iter().enumerate() {
            c.compose_ident(
                &format!("{p}.services[{i}]"),
                "Compose service",
                svc,
                false,
                false,
            );
        }
        for (i, n) in group.networks.iter().enumerate() {
            c.compose_ident(&format!("{p}.networks[{i}]"), "network", n, false, true);
        }
        for (i, a) in group.after.iter().enumerate() {
            if a == g {
                c.err(format!("{p}.after[{i}]"), "a group cannot wait for itself");
            } else if !stack.compose.contains_key(a) {
                c.err(format!("{p}.after[{i}]"), format!("no compose group `{a}`"));
            }
        }
        for svc in &group.shared {
            if !group.services.contains(svc) {
                c.err(
                    format!("{p}.shared"),
                    format!("`{svc}` is not in this group's `services`"),
                );
            }
        }
        for svc in &group.main_ports {
            if !group.services.contains(svc) {
                c.err(
                    format!("{p}.main_ports"),
                    format!("`{svc}` is not in this group's `services`"),
                );
            } else if group.shared.contains(svc) {
                // A lane has none of its own: there is nothing to move to.
                c.err(
                    format!("{p}.main_ports"),
                    format!(
                        "`{svc}` is `shared`: lanes use the main tree's, so its ports never move"
                    ),
                );
            }
        }
        for (svc, globs) in &group.restart_on {
            if !group.services.contains(svc) {
                c.err(
                    format!("{p}.restart_on.{svc}"),
                    format!("`{svc}` is not in this group's `services`"),
                );
            }
            c.globs(&format!("{p}.restart_on.{svc}"), globs);
        }
        for (svc, url) in &group.urls {
            if !group.services.contains(svc) {
                c.err(
                    format!("{p}.urls.{svc}"),
                    format!("`{svc}` is not in this group's `services`"),
                );
            } else if !url.is_empty()
                && !(url.starts_with("http://") || url.starts_with("https://"))
            {
                c.err(
                    format!("{p}.urls.{svc}"),
                    "an http:// or https:// address, or \"\" for no link",
                );
            }
        }
    }
    if let Some(cycle) = find_cycle(
        stack
            .compose
            .iter()
            .map(|(k, g)| (k.as_str(), g.after.iter().map(String::as_str).collect())),
    ) {
        c.err(
            "compose",
            format!("`after` forms a cycle: {}", cycle.join(" -> ")),
        );
    }

    // ---------------------------------------------------------------- services
    let mut ports: BTreeMap<u16, Vec<(&str, bool)>> = BTreeMap::new();
    for (s, svc) in &stack.services {
        let p = format!("services.{s}");
        c.name(&p, s);
        match svc.kind {
            Kind::Service | Kind::Job => {
                if svc.build.is_some() != svc.output.is_some() {
                    c.err(&p, "`build` and `output` go together (ADR-0002): the build's output is copied into the artifact");
                }
            }
            Kind::Devserver | Kind::Tool => {
                if svc.build.is_some() || svc.output.is_some() {
                    c.err(&p, format!("kind `{:?}` builds and runs in one process; `build`/`output` do not apply", svc.kind).to_lowercase());
                }
            }
            Kind::Static => {
                if svc.build.is_none() || svc.output.is_none() || svc.port.is_none() {
                    c.err(&p, "kind `static` needs `build`, `output` (the bundle's directory, e.g. `dist/app/browser`) and `port`");
                }
                if !svc.run.is_empty() {
                    c.err(
                        format!("{p}.run"),
                        "kind `static` has no `run`: devstack serves the built files itself",
                    );
                }
            }
        }
        if svc.kind != Kind::Static && svc.run.trim().is_empty() {
            c.err(
                format!("{p}.run"),
                "missing `run`: the command that starts the service",
            );
        }
        if svc.spa.is_some() && svc.kind != Kind::Static {
            c.err(format!("{p}.spa"), "`spa` belongs to `kind = \"static\"`");
        }
        if svc.hot_reload.is_some() && (svc.kind != Kind::Service || svc.build.is_none()) {
            c.err(
                format!("{p}.hot_reload"),
                "hot-reload mode replaces a build: only for services with `build` (a dev server reloads itself)",
            );
        }
        if let Some(h) = &svc.hot_reload {
            if h.run.trim().is_empty() {
                c.err(
                    format!("{p}.hot_reload.run"),
                    "missing `run`: the command to run",
                );
            }
            // Runs from the source tree, without an artifact.
            c.text(
                &format!("{p}.hot_reload.run"),
                &h.run,
                Scope::Service {
                    port: svc.port.is_some(),
                    artifact: false,
                },
            );
            if let Some(d) = &h.cwd {
                c.literal(&format!("{p}.hot_reload.cwd"), d);
                c.repo_path(&format!("{p}.hot_reload.cwd"), d);
            }
        }
        if let Some(prep) = &svc.prepare {
            if prep.run.trim().is_empty() {
                c.err(
                    format!("{p}.prepare.run"),
                    "missing `run`: the command to run",
                );
            }
            c.literal(&format!("{p}.prepare.run"), &prep.run);
            if let Some(d) = &prep.cwd {
                c.literal(&format!("{p}.prepare.cwd"), d);
                c.repo_path(&format!("{p}.prepare.cwd"), d);
            }
            c.globs(&format!("{p}.prepare.inputs"), &prep.inputs);
            for l in &prep.link {
                let at = format!("{p}.prepare.link");
                c.literal(&at, l);
                c.repo_path(&at, l);
                if l.trim_matches(['/', '\\', '.']).is_empty() {
                    c.err(
                        &at,
                        format!(
                            "`{l}`: a directory below the repository, not the repository itself"
                        ),
                    );
                }
            }
        }
        if svc.kind == Kind::Job && (svc.port.is_some() || svc.health.is_some()) {
            c.err(
                &p,
                "a job runs to completion; `port` and `health` do not apply",
            );
        }
        if svc.timeout.is_some() && svc.kind != Kind::Job {
            c.err(
                format!("{p}.timeout"),
                "only a job has a `timeout` (a service's start is bounded by `health.timeout`)",
            );
        }
        if svc.timeout.is_some_and(|t| t.is_zero()) {
            c.err(format!("{p}.timeout"), "must be more than 0");
        }
        if svc.kind == Kind::Tool && svc.health.is_some() {
            c.err(format!("{p}.health"), "a tool has no health check");
        }
        if svc.proxy == Some(true)
            && (!matches!(svc.kind, Kind::Service | Kind::Static | Kind::Devserver)
                || svc.port.is_none())
        {
            c.err(
                format!("{p}.proxy"),
                "only a long-running service or a dev server with a `port` can sit behind the proxy",
            );
        }
        if svc.proxy == Some(true) && svc.kind == Kind::Devserver && !svc.run_takes_port() {
            c.err(
                format!("{p}.proxy"),
                "a dev server behind devstack's forward runs on a port of its own: its `run` needs `{port}` (ADR-0043)",
            );
        }
        if svc.health.is_some() && svc.port.is_none() {
            c.err(format!("{p}.health"), "an HTTP health check needs `port`");
        }
        if svc.health.as_ref().is_some_and(|h| h.timeout.is_zero()) {
            c.err(format!("{p}.health.timeout"), "must be more than 0");
        }
        if let Some(port) = svc.port {
            if port == 0 {
                c.err(format!("{p}.port"), "must be a port from 1 to 65535");
            } else if step <= MAX_PORT_STEP && u32::from(port) + u32::from(step) > 65535 {
                c.err(
                    format!("{p}.port"),
                    format!("{port} plus `[lanes] port_step` ({step}) leaves no port for a lane"),
                );
            }
            ports
                .entry(port)
                .or_default()
                .push((s.as_str(), svc.autostart));
        }
        c.globs(&format!("{p}.watch"), &svc.watch);
        c.globs(&format!("{p}.restart_on"), &svc.restart_on);
        if let Some(r) = &svc.rebuild {
            let rp = format!("{p}.rebuild");
            if svc.kind != Kind::Devserver {
                c.err(
                    &rp,
                    "only a dev server (`kind = \"devserver\"`) rebuilds by itself",
                );
            }
            match r {
                crate::model::Rebuild::Named(n) => {
                    if n != "auto"
                        && n != "none"
                        && !crate::model::REBUILD_TOOLS.contains(&n.as_str())
                    {
                        c.err(
                            &rp,
                            format!(
                                "`{n}`: expected `auto`, `none` or one of {}",
                                crate::model::REBUILD_TOOLS.join(", ")
                            ),
                        );
                    }
                }
                crate::model::Rebuild::Lines(l) => {
                    for (k, re) in [
                        ("start", l.start.as_deref()),
                        ("ok", Some(l.ok.as_str())),
                        ("failed", Some(l.failed.as_str())),
                    ] {
                        if let Some(re) = re
                            && let Err(e) = regex::Regex::new(re)
                        {
                            c.err(
                                format!("{rp}.{k}"),
                                format!("not a regular expression: {e}"),
                            );
                        }
                    }
                }
            }
        }
        if let Some(pr) = &svc.promote {
            c.globs(&format!("{p}.promote.hold_if_changed"), &pr.hold_if_changed);
        }
        for (i, a) in svc.after.iter().enumerate() {
            let ap = format!("{p}.after[{i}]");
            if let Some(rest) = a.strip_prefix("compose.") {
                match rest.split_once('.') {
                    Some((group, service)) => c.compose_service(&ap, a, group, service),
                    None => c.err(&ap, format!("`{a}`: expected `compose.<group>.<service>`")),
                }
            } else if a == s {
                c.err(&ap, "a service cannot wait for itself");
            } else {
                c.service_ref(&ap, a);
            }
        }
        match &svc.compose {
            Some(r) if !stack.compose.contains_key(&r.group) => c.err(format!("{p}.compose.group"), format!("no compose group `{}`", r.group)),
            None if !svc.rewrite_hosts.is_empty() => c.err(format!("{p}.rewrite_hosts"), "needs `compose` (ADR-0014): hosts are rewritten in the Compose service's environment"),
            _ => {}
        }
        if let Some(r) = &svc.compose {
            // `<service>` of the service's own group, or `<group>.<service>` of another.
            for (i, h) in svc.rewrite_hosts.iter().enumerate() {
                let (group, service) = h.split_once('.').unwrap_or((r.group.as_str(), h.as_str()));
                c.compose_service(&format!("{p}.rewrite_hosts[{i}]"), h, group, service);
            }
        }
        if let Some(w) = &svc.workers
            && w.env.is_empty()
        {
            c.err(
                format!("{p}.workers.env"),
                "must name at least one variable (ADR-0008)",
            );
        }
        // Strings that are interpolated.
        let scope = Scope::Service {
            port: svc.port.is_some(),
            artifact: svc.build.is_some(),
        };
        c.text(&format!("{p}.run"), &svc.run, scope);
        for (k, field) in [("build", &svc.build), ("output", &svc.output)] {
            if let Some(v) = field {
                c.text(&format!("{p}.{k}"), v, Scope::Build);
            }
        }
        // The build's output is copied from the snapshot (ADR-0002); where
        // `{repo}` or `${VAR}` make it absolute, the builder checks that it
        // stays there.
        if let Some(o) = &svc.output {
            let rel = match o.strip_prefix("{repo}") {
                Some(r) => r.trim_start_matches(['/', '\\']),
                None => o,
            };
            if !(rel.is_empty() && o.starts_with("{repo}"))
                && !o.contains("${")
                && !inside_repo(rel)
            {
                c.err(
                    format!("{p}.output"),
                    format!("`{o}` must be a path inside the repository, relative to it"),
                );
            }
        }
        // A working directory is joined to the repository (or snapshot) as written.
        if let Some(d) = &svc.cwd {
            c.literal(&format!("{p}.cwd"), d);
            c.repo_path(&format!("{p}.cwd"), d);
        }
        for (k, v) in &svc.env {
            c.text(&format!("{p}.env.{k}"), v, scope);
        }
        if let Some(w) = &svc.workers {
            for (k, v) in &w.env {
                c.text(&format!("{p}.workers.env.{k}"), v, scope);
            }
        }
        for (i, d) in svc.ensure_dirs.iter().enumerate() {
            c.text(&format!("{p}.ensure_dirs[{i}]"), d, scope);
        }
    }
    // Two services on one port are fine only if at most one starts by itself
    // (plan §4.7: a dev server and an on-demand host for the same SPA).
    for (port, users) in &ports {
        if users.iter().filter(|(_, auto)| *auto).count() > 1 {
            let names: Vec<_> = users.iter().map(|(n, _)| *n).collect();
            c.err(
                "services",
                format!(
                    "port {port} is used by {} which all start automatically",
                    names.join(", ")
                ),
            );
        }
    }
    if let Some(cycle) = find_cycle(stack.services.iter().map(|(k, s)| {
        (
            k.as_str(),
            s.after
                .iter()
                .filter(|a| !a.starts_with("compose."))
                .map(String::as_str)
                .collect(),
        )
    })) {
        c.err(
            "services",
            format!("`after` forms a cycle: {}", cycle.join(" -> ")),
        );
    }

    // ---------------------------------------------------------------- commands
    for (n, cmd) in &stack.commands {
        let p = format!("commands.{n}");
        c.name(&p, n);
        if cmd.steps.is_empty() {
            c.err(format!("{p}.steps"), "needs at least one step");
        }
        for (i, r) in cmd.requires.iter().enumerate() {
            c.service_ref(&format!("{p}.requires[{i}]"), r);
        }
        for (i, step) in cmd.steps.iter().enumerate() {
            let sp = format!("{p}.steps[{i}]");
            let action = step.action();
            // Keys that belong to another action would be ignored silently.
            let is_exec = matches!(action, Ok(StepAction::Exec { .. }));
            let is_http = matches!(action, Ok(StepAction::Http { .. }));
            if action.is_ok() {
                if step.cwd.is_some() && !is_exec {
                    c.err(format!("{sp}.cwd"), "`cwd` belongs to an `exec` step");
                }
                if step.timeout.is_some() && !is_exec && !is_http {
                    c.err(
                        format!("{sp}.timeout"),
                        "`timeout` belongs to an `exec` or `http` step",
                    );
                }
                for (key, set) in [
                    ("headers", !step.headers.is_empty()),
                    ("body", step.body.is_some()),
                ] {
                    if set && !is_http {
                        c.err(
                            format!("{sp}.{key}"),
                            format!("`{key}` belongs to an `http` step"),
                        );
                    }
                }
            }
            match action {
                Err(set) if set.is_empty() => {
                    c.err(&sp, "needs one of exec, http, start, stop, restart, run")
                }
                Err(set) => c.err(
                    &sp,
                    format!("has several actions ({}); one per step", set.join(", ")),
                ),
                Ok(StepAction::Exec { command, cwd }) => {
                    c.text(&format!("{sp}.exec"), command, Scope::Command);
                    if let Some(d) = cwd {
                        c.text(&format!("{sp}.cwd"), d, Scope::Command);
                    }
                }
                Ok(StepAction::Http { method, url }) => {
                    const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
                    if !METHODS.contains(&method) {
                        c.err(
                            format!("{sp}.http"),
                            format!(
                                "expected `METHOD url` with METHOD one of {}",
                                METHODS.join(", ")
                            ),
                        );
                    }
                    c.text(&format!("{sp}.http"), url, Scope::Command);
                    for (k, v) in &step.headers {
                        c.text(&format!("{sp}.headers.{k}"), v, Scope::Command);
                    }
                    if let Some(b) = &step.body {
                        c.text(&format!("{sp}.body"), b, Scope::Command);
                    }
                }
                Ok(StepAction::Start(s) | StepAction::Stop(s) | StepAction::Restart(s)) => {
                    c.service_ref(&sp, s);
                }
                Ok(StepAction::Run(s)) => {
                    if c.service_ref(&sp, s) && stack.services[s].kind != Kind::Job {
                        c.err(
                            &sp,
                            format!("`run` needs a service of kind `job`; `{s}` is not one"),
                        );
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ groups
    for (n, group) in &stack.groups {
        let p = format!("groups.{n}");
        c.name(&p, n);
        if group.members.is_empty() {
            c.err(format!("{p}.members"), "needs at least one member");
        }
        let mut seen = BTreeSet::new();
        for (i, m) in group.parsed_members().into_iter().enumerate() {
            let mp = format!("{p}.members[{i}]");
            match m {
                Err(raw) => c.err(&mp, format!("`{raw}`: expected `<service>`, `compose.<group>.<service>` or `command.<name>`")),
                Ok(member) => {
                    if !seen.insert(group.members[i].clone()) {
                        c.err(&mp, format!("`{}` is listed twice", group.members[i]));
                    }
                    match member {
                        Member::Service(s) => {
                            c.service_ref(&mp, s);
                        }
                        Member::ComposeService { group: g, service } => c.compose_service(&mp, &group.members[i], g, service),
                        Member::Command(cmd) => {
                            if !stack.commands.contains_key(cmd) {
                                c.err(&mp, format!("no command `{cmd}`"));
                            }
                        }
                    }
                }
            }
        }
    }

    c.issues
}

/// Lanes up to this number are worth a warning when their ports collide; a
/// collision further out (5000 and 4200 at step 100: lane 8) costs a lane
/// number nobody misses.
const FIRST_LANES: u32 = 3;

/// What a valid stack file may want changed (`devstack check` prints them):
/// two services whose ports differ by one of the first few multiples of
/// `[lanes] port_step`, so that one of the first lanes would run one of
/// them on the other's port. The daemon skips such lane numbers, so lane 1
/// is not on `port + port_step` as the file suggests.
pub fn warnings(stack: &Stack) -> Vec<Issue> {
    let step = u32::from(stack.lanes.port_step);
    let mut out = Vec::new();
    if step == 0 {
        return out;
    }
    let ports: Vec<(&str, u32)> = stack
        .services
        .iter()
        .filter_map(|(n, s)| s.port.map(|p| (n.as_str(), u32::from(p))))
        .collect();
    for (i, &(a, pa)) in ports.iter().enumerate() {
        for &(b, pb) in &ports[i + 1..] {
            let (low, lp, high, hp) = if pa <= pb {
                (a, pa, b, pb)
            } else {
                (b, pb, a, pa)
            };
            let lane = (hp - lp) / step;
            if hp != lp && (hp - lp) % step == 0 && lane <= FIRST_LANES {
                out.push(Issue {
                    path: format!("services.{high}.port"),
                    message: format!(
                        "lane {lane} would run `{low}` on {hp}, this service's port ({lp} + {lane} x `[lanes] port_step` {step}): the daemon skips that lane number; choose ports whose difference is no multiple of {step}"
                    ),
                });
            }
        }
    }
    out
}

/// What `devstack check` warns about from the repository's files (`repo`,
/// the stack file's directory): a dev server a lane runs whose `cwd` has a
/// `package.json` and which has no `prepare`. A lane runs it from a copy
/// without the ignored files (ADR-0024), `node_modules` among them, and it
/// fails there with only its toolchain's message ("vite is not recognized").
pub fn repo_warnings(stack: &Stack, repo: &std::path::Path) -> Vec<Issue> {
    let mut out = Vec::new();
    for (name, svc) in &stack.services {
        if svc.kind != Kind::Devserver || !svc.lanes || svc.prepare.is_some() {
            continue;
        }
        let cwd = svc.cwd.as_deref().unwrap_or(".");
        if cwd.contains('{') || cwd.contains("${") {
            continue;
        }
        if repo.join(cwd).join("package.json").is_file() {
            let lock = match cwd.trim_end_matches(['/', '\\']) {
                "." | "" => "package-lock.json".to_string(),
                dir => format!("{}/package-lock.json", dir.replace('\\', "/")),
            };
            out.push(Issue {
                path: format!("services.{name}"),
                message: format!(
                    "`{cwd}` has a package.json and the dev server no `prepare`: a lane runs it from a copy without the ignored files (node_modules), where it fails; add `prepare = {{ run = \"npm ci\", inputs = [\"{lock}\"] }}`"
                ),
            });
        }
    }
    out
}

/// First cycle in a dependency graph, as a path that starts and ends at the same node.
fn find_cycle<'a>(edges: impl Iterator<Item = (&'a str, Vec<&'a str>)>) -> Option<Vec<String>> {
    let graph: BTreeMap<&str, Vec<&str>> = edges.collect();
    fn visit<'a>(
        n: &'a str,
        g: &BTreeMap<&'a str, Vec<&'a str>>,
        stack: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(i) = stack.iter().position(|&x| x == n) {
            let mut cyc: Vec<String> = stack[i..].iter().map(|s| s.to_string()).collect();
            cyc.push(n.to_string());
            return Some(cyc);
        }
        if !done.insert(n) {
            return None;
        }
        stack.push(n);
        for &next in g.get(n).map(Vec::as_slice).unwrap_or_default() {
            if let Some(c) = visit(next, g, stack, done) {
                return Some(c);
            }
        }
        stack.pop();
        None
    }
    let mut done = BTreeSet::new();
    for &n in graph.keys() {
        if let Some(c) = visit(n, &graph, &mut Vec::new(), &mut done) {
            return Some(c);
        }
    }
    None
}

/// `[lanes.need]`, a profile's `need` (ADR-0049 point 5).
fn check_need(c: &mut Check<'_>, path: &str, n: &crate::model::Need) {
    if let Some(cpus) = n.cpus
        && !(cpus.is_finite() && cpus > 0.0)
    {
        c.err(
            format!("{path}.cpus"),
            "must be a number of CPU cores above 0",
        );
    }
    if let Some(g) = &n.gpus
        && g.count == 0
    {
        c.err(
            format!("{path}.gpus.count"),
            "must be at least 1 (leave `gpus` out for none)",
        );
    }
}
