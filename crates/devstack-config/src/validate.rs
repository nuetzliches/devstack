//! Cross-reference checks that the deserialiser cannot do. All problems are
//! collected and reported together, each with the path of the offending key.

use crate::interpolate::{self, Placeholder};
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
        }
    }

    /// Syntax and references of every `{…}` placeholder in a string.
    fn text(&mut self, path: &str, s: &str) {
        let segs = match interpolate::parse(s) {
            Ok(segs) => segs,
            Err(e) => return self.err(path, e.to_string()),
        };
        for p in interpolate::placeholders(&segs) {
            match p {
                Placeholder::ServiceUrl(name) => match self.stack.services.get(name) {
                    None => self.err(path, format!("`{p}`: no service `{name}`")),
                    Some(svc) if svc.port.is_none() => {
                        self.err(path, format!("`{p}`: service `{name}` has no port"))
                    }
                    _ => {}
                },
                Placeholder::ComposePort { group, service, .. } => {
                    self.compose_service(path, &format!("{p}"), group, service)
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

    if stack.version != SCHEMA_VERSION {
        c.err(
            "version",
            format!(
                "unsupported version {} (this devstack reads version {SCHEMA_VERSION})",
                stack.version
            ),
        );
    }
    if stack.stack.name.trim().is_empty() {
        c.err("stack.name", "must not be empty");
    }

    // ---------------------------------------------------------- compose groups
    let mut projects = BTreeMap::new();
    for (g, group) in &stack.compose {
        let p = format!("compose.{g}");
        c.name(&p, g);
        if group.project.trim().is_empty() {
            c.err(format!("{p}.project"), "must not be empty");
        } else if let Some(other) = projects.insert(group.project.clone(), g) {
            c.err(
                format!("{p}.project"),
                format!(
                    "project `{}` is also used by group `{other}`",
                    group.project
                ),
            );
        }
        if group.files.is_empty() {
            c.err(format!("{p}.files"), "needs at least one Compose file");
        }
        if group.services.is_empty() {
            c.err(
                format!("{p}.services"),
                "needs at least one service to start",
            );
        }
        for (i, a) in group.after.iter().enumerate() {
            if a == g {
                c.err(format!("{p}.after[{i}]"), "a group cannot wait for itself");
            } else if !stack.compose.contains_key(a) {
                c.err(format!("{p}.after[{i}]"), format!("no compose group `{a}`"));
            }
        }
        for svc in group.restart_on.keys() {
            if !group.services.contains(svc) {
                c.err(
                    format!("{p}.restart_on.{svc}"),
                    format!("`{svc}` is not in this group's `services`"),
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
        }
        if svc.kind == Kind::Job && (svc.port.is_some() || svc.health.is_some()) {
            c.err(
                &p,
                "a job runs to completion; `port` and `health` do not apply",
            );
        }
        if svc.kind == Kind::Tool && svc.health.is_some() {
            c.err(format!("{p}.health"), "a tool has no health check");
        }
        if svc.health.is_some() && svc.port.is_none() {
            c.err(format!("{p}.health"), "an HTTP health check needs `port`");
        }
        if let Some(port) = svc.port {
            ports
                .entry(port)
                .or_default()
                .push((s.as_str(), svc.autostart));
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
        c.text(&format!("{p}.run"), &svc.run);
        for (k, field) in [
            ("build", &svc.build),
            ("output", &svc.output),
            ("cwd", &svc.cwd),
        ] {
            if let Some(v) = field {
                c.text(&format!("{p}.{k}"), v);
            }
        }
        for (k, v) in &svc.env {
            c.text(&format!("{p}.env.{k}"), v);
        }
        if let Some(w) = &svc.workers {
            for (k, v) in &w.env {
                c.text(&format!("{p}.workers.env.{k}"), v);
            }
        }
        for (i, d) in svc.ensure_dirs.iter().enumerate() {
            c.text(&format!("{p}.ensure_dirs[{i}]"), d);
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
            match step.action() {
                Err(set) if set.is_empty() => {
                    c.err(&sp, "needs one of exec, http, start, stop, restart, run")
                }
                Err(set) => c.err(
                    &sp,
                    format!("has several actions ({}); one per step", set.join(", ")),
                ),
                Ok(StepAction::Exec { command, cwd }) => {
                    c.text(&format!("{sp}.exec"), command);
                    if let Some(d) = cwd {
                        c.text(&format!("{sp}.cwd"), d);
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
                    c.text(&format!("{sp}.http"), url);
                    for (k, v) in &step.headers {
                        c.text(&format!("{sp}.headers.{k}"), v);
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
