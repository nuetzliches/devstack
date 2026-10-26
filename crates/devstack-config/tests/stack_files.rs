use devstack_config::{LoadError, from_str, load, model::Kind};
use std::path::Path;

fn invalid(text: &str) -> Vec<String> {
    match from_str(text, Path::new("test.toml")) {
        Err(LoadError::Invalid { issues, .. }) => issues.iter().map(|i| i.to_string()).collect(),
        Err(e) => panic!("expected validation issues, got {e}"),
        Ok(_) => panic!("expected validation issues, got a valid stack"),
    }
}

const MINIMAL: &str = r#"
version = 1
[stack]
name = "t"
[compose.app]
project = "t-dev"
files = ["compose.yml"]
services = ["db", "queue"]
[services.api]
build = "dotnet build"
output = "bin"
run = "{artifact}/Api"
port = 5000
"#;

#[test]
fn the_published_example_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/devstack.toml");
    let stack = load(&path).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(stack.services["migrate"].kind, Kind::Job);
    assert!(stack.services["tray-helper"].platforms.len() == 1);
    assert_eq!(stack.commands["reset-db"].steps.len(), 4);
}

#[test]
fn minimal_stack_is_valid() {
    from_str(MINIMAL, Path::new("t")).unwrap();
}

#[test]
fn unknown_keys_are_errors_with_location() {
    let err = from_str(&format!("{MINIMAL}\nbiuld = \"x\"\n"), Path::new("t"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("biuld"), "{err}");
    let err = from_str(
        &MINIMAL.replace("port = 5000", "port = 5000\nprot = 1"),
        Path::new("t"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("prot") && err.contains("line"), "{err}");
}

#[test]
fn wrong_version() {
    let issues = invalid(&MINIMAL.replace("version = 1", "version = 2"));
    assert!(issues[0].contains("unsupported version 2"), "{issues:?}");
}

#[test]
fn build_needs_output() {
    let issues = invalid(&MINIMAL.replace("output = \"bin\"\n", ""));
    assert!(
        issues
            .iter()
            .any(|i| i.contains("`build` and `output` go together")),
        "{issues:?}"
    );
}

#[test]
fn only_a_job_has_a_timeout() {
    let job = format!(
        "{MINIMAL}[services.migrate]
kind = \"job\"
run = \"x\"
timeout = \"10m\"
"
    );
    let stack = from_str(&job, Path::new("t")).unwrap();
    assert_eq!(
        stack.services["migrate"].timeout,
        Some(std::time::Duration::from_secs(600))
    );
    let issues = invalid(&MINIMAL.replace(
        "port = 5000",
        "port = 5000
timeout = \"1m\"",
    ));
    assert!(
        issues
            .iter()
            .any(|i| i.contains("only a job has a `timeout`")),
        "{issues:?}"
    );
}

#[test]
fn references_are_checked() {
    let text = format!(
        "{MINIMAL}after = [\"nope\", \"compose.app.cache\", \"compose.other.db\"]\nenv = {{ A = \"{{service.ghost.url}}\", B = \"{{compose.app.cache.80}}\", C = \"{{bogus}}\" }}\n"
    );
    let issues = invalid(&text);
    let all = issues.join("\n");
    assert!(
        all.contains("services.api.after[0]: no service `nope`"),
        "{all}"
    );
    assert!(
        all.contains("`cache` is not started by compose group `app`"),
        "{all}"
    );
    assert!(all.contains("no compose group `other`"), "{all}");
    assert!(
        all.contains("services.api.env.A: `{service.ghost.url}`: no service `ghost`"),
        "{all}"
    );
    assert!(
        all.contains("services.api.env.C: unknown placeholder `{bogus}`"),
        "{all}"
    );
}

#[test]
fn shared_port_allowed_only_when_one_is_on_demand() {
    let two = format!(
        "{MINIMAL}\n[services.web]\nkind = \"devserver\"\nrun = \"ng serve\"\nport = 5000\n"
    );
    assert!(invalid(&two).iter().any(|i| i.contains("port 5000")));
    let on_demand = format!("{two}autostart = false\n");
    from_str(&on_demand, Path::new("t")).unwrap();
}

#[test]
fn cycles_are_reported() {
    let text = format!(
        "{MINIMAL}after = [\"b\"]\n[services.b]\nkind = \"tool\"\nrun = \"x\"\nafter = [\"api\"]\n"
    );
    let issues = invalid(&text);
    assert!(issues.iter().any(|i| i.contains("cycle")), "{issues:?}");
}

#[test]
fn command_steps() {
    let text = format!(
        "{MINIMAL}\n[commands.x]\ndescription = \"d\"\nsteps = [{{ start = \"api\", stop = \"api\" }}, {{ run = \"api\" }}, {{ http = \"FETCH /x\" }}, {{}}]\n"
    );
    let all = invalid(&text).join("\n");
    assert!(
        all.contains("steps[0]: has several actions (start, stop)"),
        "{all}"
    );
    assert!(
        all.contains("steps[1]: `run` needs a service of kind `job`"),
        "{all}"
    );
    assert!(
        all.contains("steps[2].http: expected `METHOD url`"),
        "{all}"
    );
    assert!(all.contains("steps[3]: needs one of"), "{all}");
}

#[test]
fn rewrite_hosts_need_a_compose_source() {
    let issues = invalid(&MINIMAL.replace("port = 5000", "port = 5000\nrewrite_hosts = [\"db\"]"));
    assert!(
        issues.iter().any(|i| i.contains("needs `compose`")),
        "{issues:?}"
    );
    let ok = MINIMAL.replace(
        "port = 5000",
        "port = 5000\ncompose = { group = \"app\", service = \"api\" }\nrewrite_hosts = [\"db\"]",
    );
    from_str(&ok, Path::new("t")).unwrap();
    let bad = ok.replace("rewrite_hosts = [\"db\"]", "rewrite_hosts = [\"mail\"]");
    assert!(
        invalid(&bad)
            .iter()
            .any(|i| i.contains("`mail` is not started"))
    );
    let other_group = format!(
        "{ok}
[compose.search]
project = \"t-search\"
files = [\"s.yml\"]
services = [\"es\"]
"
    )
    .replace(
        "rewrite_hosts = [\"db\"]",
        "rewrite_hosts = [\"db\", \"search.es\"]",
    );
    from_str(&other_group, Path::new("t")).unwrap();
}

#[test]
fn durations() {
    let ok = MINIMAL.replace(
        "port = 5000",
        "port = 5000\nhealth = { http = \"/health\", timeout = \"90s\" }",
    );
    let s = from_str(&ok, Path::new("t")).unwrap();
    assert_eq!(
        s.services["api"].health.as_ref().unwrap().timeout.as_secs(),
        90
    );
    let bad = ok.replace("90s", "90 parsecs");
    let err = from_str(&bad, Path::new("t")).unwrap_err().to_string();
    assert!(err.contains("invalid duration"), "{err}");
}

#[test]
fn json_schema_names_the_top_level_tables() {
    let schema = devstack_config::json_schema();
    let props = &schema["properties"];
    for key in ["version", "stack", "compose", "services", "commands"] {
        assert!(props.get(key).is_some(), "missing {key}");
    }
}

#[test]
fn groups_are_views_with_checked_members() {
    let ok = format!(
        "{MINIMAL}
[commands.reset]
description = \"d\"
steps = [{{ exec = \"x\" }}]
[groups.core]
members = [\"api\", \"compose.app.db\", \"command.reset\"]
[groups.data]
label = \"Data\"
order = 1
members = [\"api\", \"compose.app.queue\"]
"
    );
    let s = from_str(&ok, Path::new("t")).unwrap();
    assert_eq!(s.groups.len(), 2, "a member may be in several groups");
    let bad = ok.replace(
        "\"command.reset\"]",
        "\"command.nope\", \"ghost\", \"compose.app.cache\", \"a.b\", \"api\"]",
    );
    let all = invalid(&bad).join(
        "
",
    );
    assert!(all.contains("no command `nope`"), "{all}");
    assert!(all.contains("no service `ghost`"), "{all}");
    assert!(
        all.contains("`cache` is not started by compose group `app`"),
        "{all}"
    );
    assert!(all.contains("`a.b`: expected"), "{all}");
    assert!(all.contains("`api` is listed twice"), "{all}");
}

#[test]
fn hot_reload_only_for_built_services() {
    let ok = format!("{MINIMAL}hot_reload = {{ run = \"dotnet watch run\" }}\n");
    let s = from_str(&ok, Path::new("t")).unwrap();
    assert_eq!(
        s.services["api"].hot_reload.as_ref().unwrap().run,
        "dotnet watch run"
    );
    let dev = format!(
        "{MINIMAL}[services.web]\nkind = \"devserver\"\nrun = \"npm start\"\nhot_reload = {{ run = \"x\" }}\n"
    );
    let issues = invalid(&dev);
    assert!(
        issues.iter().any(|i| i.contains("services.web.hot_reload")),
        "{issues:?}"
    );
}

#[test]
fn a_static_site_needs_a_build_output_and_port_but_no_run() {
    let ok = format!(
        "{MINIMAL}[services.web]\nkind = \"static\"\nbuild = \"npm run build\"\noutput = \"dist/web/browser\"\nport = 4300\n"
    );
    from_str(&ok, Path::new("t")).unwrap();
    let with_run = ok.replace("port = 4300\n", "port = 4300\nrun = \"x\"\n");
    assert!(
        invalid(&with_run)
            .iter()
            .any(|i| i.contains("services.web.run"))
    );
    let no_port = ok.replace("port = 4300\n", "");
    assert!(
        invalid(&no_port)
            .iter()
            .any(|i| i.contains("kind `static` needs"))
    );
    let missing_run = format!("{MINIMAL}[services.job]\nkind = \"job\"\n");
    assert!(
        invalid(&missing_run)
            .iter()
            .any(|i| i.contains("missing `run`"))
    );
}

#[test]
fn a_lane_port_step_of_zero_is_an_error() {
    let issues = invalid(&format!("{MINIMAL}\n[lanes]\nport_step = 0\n"));
    assert!(
        issues.iter().any(|i| i.contains("lanes.port_step")),
        "{issues:?}"
    );
}

#[test]
fn lane_sync_globs_stay_inside_the_repository() {
    let issues = invalid(&format!(
        "{MINIMAL}\n[lanes]\nsync = [\"certs/*.pem\", \"../secrets/*\", \"a/[b\"]\n"
    ));
    let sync: Vec<_> = issues.iter().filter(|i| i.contains("lanes.sync")).collect();
    assert_eq!(sync.len(), 2, "{issues:?}");
    assert!(sync.iter().any(|i| i.contains("../secrets")), "{issues:?}");
}

fn has(issues: &[String], path: &str, text: &str) -> bool {
    issues
        .iter()
        .any(|i| i.starts_with(path) && i.contains(text))
}

#[test]
fn a_newer_schema_version_says_so_before_its_unknown_keys() {
    let issues = invalid(&format!(
        "{}\n[future]\nkey = 1\n",
        MINIMAL.replace("version = 1", "version = 2")
    ));
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].contains("unsupported version 2"), "{issues:?}");
}

#[test]
fn paths_of_compose_and_env_files_stay_inside_the_repository() {
    let text = MINIMAL
        .replace(
            "files = [\"compose.yml\"]",
            r#"files = ["../other/compose.yml", "/etc/c.yml", 'C:\c.yml']
env_files = ['..\x\.env']"#,
        )
        .replace(
            "name = \"t\"",
            "name = \"t\"\nenv_files = [\"/home/me/.env\"]",
        );
    let issues = invalid(&text);
    for p in [
        "compose.app.files[0]",
        "compose.app.files[1]",
        "compose.app.files[2]",
        "compose.app.env_files[0]",
        "stack.env_files[0]",
    ] {
        assert!(has(&issues, p, "inside the repository"), "{p}: {issues:?}");
    }
    let issues = invalid(&format!(
        "{MINIMAL}\n[lanes]\ndockerfile = \"/etc/Dockerfile\"\n"
    ));
    assert!(has(&issues, "lanes.dockerfile", "inside"), "{issues:?}");
    let issues = invalid(&format!(
        "{MINIMAL}{}",
        r"
[lanes]
dockerfile = 'C:\Dockerfile'
"
    ));
    assert!(has(&issues, "lanes.dockerfile", "inside"), "{issues:?}");
}

#[test]
fn ports_and_the_port_step_are_bounded() {
    let issues = invalid(&MINIMAL.replace("port = 5000", "port = 0"));
    assert!(
        has(&issues, "services.api.port", "1 to 65535"),
        "{issues:?}"
    );
    let issues = invalid(&format!("{MINIMAL}\n[lanes]\nport_step = 20000\n"));
    assert!(has(&issues, "lanes.port_step", "at most"), "{issues:?}");
    let issues = invalid(&MINIMAL.replace("port = 5000", "port = 65500"));
    assert!(has(&issues, "services.api.port", "port_step"), "{issues:?}");
}

#[test]
fn windows_device_names_are_no_names() {
    let issues = invalid(&MINIMAL.replace("[services.api]", "[services.con]"));
    assert!(has(&issues, "services.con", "device name"), "{issues:?}");
    let issues = invalid(&MINIMAL.replace("[compose.app]", "[compose.nul]"));
    assert!(has(&issues, "compose.nul", "device name"), "{issues:?}");
}

#[test]
fn compose_names_follow_compose() {
    let issues = invalid(
        &MINIMAL
            .replace("project = \"t-dev\"", "project = \"-T dev\"")
            .replace(
                "services = [\"db\", \"queue\"]",
                "services = [\"db.x\", \"-q\"]\nnetworks = [\"--net\"]",
            ),
    );
    for p in [
        "compose.app.project",
        "compose.app.services[0]",
        "compose.app.services[1]",
        "compose.app.networks[0]",
    ] {
        assert!(
            has(&issues, p, "starts with a letter or digit"),
            "{p}: {issues:?}"
        );
    }
}

#[test]
fn placeholders_need_a_value_where_they_are_used() {
    let text = format!(
        "{MINIMAL}\n[services.tool]\nkind = \"tool\"\nrun = \"x --port {{port}} {{artifact}}\"\n"
    );
    let issues = invalid(&text);
    assert!(
        has(
            &issues,
            "services.tool.run",
            "`{port}`: the service has no `port`"
        ),
        "{issues:?}"
    );
    assert!(
        has(&issues, "services.tool.run", "`{artifact}`"),
        "{issues:?}"
    );

    let issues = invalid(&MINIMAL.replace(
        "build = \"dotnet build\"",
        "build = \"npm run build -- --base-href=/{lane}/\"",
    ));
    assert!(
        has(&issues, "services.api.build", "not known to a build"),
        "{issues:?}"
    );
    // `{repo}` and `${VAR}` are what a build takes.
    from_str(
        &MINIMAL.replace(
            "build = \"dotnet build\"",
            "build = \"dotnet build {repo}/x ${CONFIG:-Debug}\"",
        ),
        Path::new("t"),
    )
    .unwrap();

    let issues = invalid(&MINIMAL.replace("port = 5000", "port = 5000\ncwd = \"{repo}/web\""));
    assert!(
        has(&issues, "services.api.cwd", "not expanded"),
        "{issues:?}"
    );

    let issues = invalid(&format!(
        "{MINIMAL}\n[commands.c]\ndescription = \"d\"\nsteps = [{{ exec = \"echo {{port}} {{compose.app.db.5432}}\" }}]\n"
    ));
    assert!(
        has(
            &issues,
            "commands.c.steps[0].exec",
            "`{port}`: is not available"
        ),
        "{issues:?}"
    );
    assert!(
        !issues.iter().any(|i| i.contains("{compose.app.db.5432}")),
        "a command may use a Compose service's address: {issues:?}"
    );

    // A group's project as this tree runs it, for `docker compose -p`.
    from_str(
        &format!(
            "{MINIMAL}\n[commands.c]\ndescription = \"d\"\nsteps = [{{ exec = \"docker compose -p {{compose.app.project}} exec -T db true\" }}]\n"
        ),
        Path::new("t"),
    )
    .unwrap();
    let issues = invalid(&format!(
        "{MINIMAL}\n[commands.c]\ndescription = \"d\"\nsteps = [{{ exec = \"docker compose -p {{compose.other.project}} ps\" }}]\n"
    ));
    assert!(
        has(
            &issues,
            "commands.c.steps[0].exec",
            "`{compose.other.project}`: no compose group `other`"
        ),
        "{issues:?}"
    );
}

#[test]
fn hot_reload_and_prepare_are_checked_like_the_rest() {
    let text = MINIMAL.replace(
        "port = 5000",
        "port = 5000\nhot_reload = { run = \"dotnet watch {nope}\", cwd = \"{repo}\" }\nprepare = { run = \"npm ci --prefix ${DIR}\", cwd = \"{repo}\", inputs = [\"a/{b\"] }\nwatch = [\"src/[\"]\nrestart_on = [\"x/{\"]\npromote = { hold_if_changed = [\"m/[\"] }",
    );
    let issues = invalid(&text);
    for (p, t) in [
        ("services.api.hot_reload.run", "unknown placeholder"),
        ("services.api.hot_reload.cwd", "not expanded"),
        ("services.api.prepare.run", "not expanded"),
        ("services.api.prepare.cwd", "not expanded"),
        ("services.api.prepare.inputs", "a/{b"),
        ("services.api.watch", "src/["),
        ("services.api.restart_on", "x/{"),
        ("services.api.promote.hold_if_changed", "m/["),
    ] {
        assert!(has(&issues, p, t), "{p}: {issues:?}");
    }
    let issues = invalid(&MINIMAL.replace(
        "services = [\"db\", \"queue\"]",
        "services = [\"db\", \"queue\"]\nrestart_on = { db = [\"init/[\"] }",
    ));
    assert!(
        has(&issues, "compose.app.restart_on.db", "init/["),
        "{issues:?}"
    );
}

#[test]
fn keys_of_another_step_kind_are_errors() {
    let issues = invalid(&format!(
        "{MINIMAL}\n[commands.c]\ndescription = \"d\"\nsteps = [{{ start = \"api\", cwd = \"x\", timeout = \"5s\" }}, {{ exec = \"x\", timeout = \"5s\", body = \"b\" }}]\n"
    ));
    assert!(
        has(&issues, "commands.c.steps[0].cwd", "`exec`"),
        "{issues:?}"
    );
    assert!(
        has(&issues, "commands.c.steps[0].timeout", "`exec` or `http`"),
        "{issues:?}"
    );
    assert!(
        !issues.iter().any(|i| i.contains("steps[1].timeout")),
        "{issues:?}"
    );
    assert!(
        has(&issues, "commands.c.steps[1].body", "`http`"),
        "{issues:?}"
    );
}

#[test]
fn an_overlong_duration_is_an_error_not_a_wrap() {
    assert!(devstack_config::model::parse_duration("99999999999999999h").is_err());
    assert!(devstack_config::model::parse_duration("999999999999999999m").is_err());
}

#[test]
fn a_compose_service_is_named_as_a_member_is_or_by_itself() {
    // #128: `devstack restart app.web`, `compose.app.web` and `web`.
    let text = r#"
version = 1
[stack]
name = "t"
[compose.app]
project = "t-dev"
files = ["compose.yml"]
services = ["db", "web", "worker"]
[compose.tools]
project = "t-tools"
files = ["tools.yml"]
services = ["db", "mail"]
[services.api]
build = "dotnet build"
output = "bin"
run = "{artifact}/Api"
port = 5000
[groups.server]
members = ["compose.app.web", "api", "compose.app.worker"]
"#;
    let s = from_str(text, Path::new("t")).unwrap();
    let pair = |g: &str, s: &str| Some((g.to_string(), s.to_string()));
    assert_eq!(s.compose_service("app.web"), pair("app", "web"));
    assert_eq!(s.compose_service("compose.app.web"), pair("app", "web"));
    assert_eq!(s.compose_service("web"), pair("app", "web"));
    assert_eq!(s.compose_service("mail"), pair("tools", "mail"));
    // A service the group does not start, named with its group: the
    // container routes tell it apart.
    assert_eq!(s.compose_service("app.cache"), pair("app", "cache"));
    // `db` is in two groups: only with its group.
    assert_eq!(s.compose_service("db"), None);
    assert_eq!(s.compose_service("tools.db"), pair("tools", "db"));
    for none in ["nope.web", "compose.web", "app.", "app.web.x", "cache", ""] {
        assert_eq!(s.compose_service(none), None, "{none}");
    }
    assert_eq!(
        s.group_containers("server"),
        vec![
            ("app".to_string(), "web".to_string()),
            ("app".to_string(), "worker".to_string())
        ]
    );
    assert!(s.group_containers("nope").is_empty());
}

#[test]
fn a_dev_server_says_how_it_rebuilds() {
    use devstack_config::model::Rebuild;
    let dev = |rebuild: &str| {
        format!(
            "{MINIMAL}[services.web]\nkind = \"devserver\"\nrun = \"npm start\"\nport = 4200\n{rebuild}\n"
        )
    };
    let stack = from_str(&dev(""), Path::new("t")).unwrap();
    assert_eq!(stack.services["web"].rebuild, None, "auto by default");
    for named in [
        "auto",
        "none",
        "angular",
        "vite",
        "webpack",
        "next",
        "dotnet-watch",
        "tsc",
    ] {
        from_str(&dev(&format!("rebuild = \"{named}\"")), Path::new("t"))
            .unwrap_or_else(|e| panic!("{named}: {e}"));
    }
    // Its own lines, as TOML literal strings: a backslash stays one.
    let own = from_str(
        &dev(r"rebuild = { start = '^rebuild', ok = '^done in (?P<ms>\d+)', failed = '^broken' }"),
        Path::new("t"),
    )
    .unwrap();
    match &own.services["web"].rebuild {
        Some(Rebuild::Lines(l)) => assert_eq!(l.ok, r"^done in (?P<ms>\d+)"),
        other => panic!("{other:?}"),
    }
    let issues = invalid(&dev("rebuild = \"gulp\""));
    assert!(
        issues.iter().any(|i| i.contains("`gulp`: expected")),
        "{issues:?}"
    );
    let issues = invalid(&dev("rebuild = { ok = '(unclosed', failed = 'x' }"));
    assert!(
        issues
            .iter()
            .any(|i| i.contains("rebuild.ok") && i.contains("not a regular expression")),
        "{issues:?}"
    );
    let issues = invalid(&MINIMAL.replace("port = 5000", "port = 5000\nrebuild = \"auto\""));
    assert!(
        issues.iter().any(|i| i.contains("only a dev server")),
        "{issues:?}"
    );
}

#[test]
fn a_dev_server_with_a_port_of_its_own_is_fronted() {
    let dev = |run: &str, extra: &str| {
        format!(
            "{MINIMAL}[services.web]\nkind = \"devserver\"\nrun = \"{run}\"\nport = 4200\n{extra}\n"
        )
    };
    let fronted = |text: String| from_str(&text, Path::new("t")).unwrap().services["web"].fronted();
    assert!(
        fronted(dev("ng serve --port {port}", "")),
        "default with `{{port}}`"
    );
    assert!(
        !fronted(dev("ng serve", "")),
        "it binds the fixed port itself"
    );
    assert!(!fronted(dev("ng serve --port {port}", "proxy = false")));
    let issues = invalid(&dev("ng serve", "proxy = true"));
    assert!(
        issues.iter().any(|i| i.contains("needs `{port}`")),
        "{issues:?}"
    );
    // The literal text `{{port}}` is no placeholder.
    assert!(!fronted(dev("ng serve --text {{port}}", "")));
    let issues = invalid(&dev("ng serve --text {{port}}", "proxy = true"));
    assert!(
        issues.iter().any(|i| i.contains("needs `{port}`")),
        "{issues:?}"
    );
}

#[test]
fn prepare_links_are_directories_inside_the_repository() {
    let ok = MINIMAL.replace(
        "port = 5000",
        "port = 5000
prepare = { run = \"npm ci\", link = [\"web/node_modules\"] }",
    );
    let stack = from_str(&ok, Path::new("test.toml")).expect("loads");
    let link = &stack.services["api"].prepare.as_ref().unwrap().link;
    assert_eq!(link, &["web/node_modules"]);
    for bad in [
        "../node_modules",
        "/abs",
        "C:/x",
        ".",
        "{repo}/node_modules",
    ] {
        let issues = invalid(&MINIMAL.replace(
            "port = 5000",
            &format!(
                "port = 5000
prepare = {{ run = \"npm ci\", link = [\"{bad}\"] }}"
            ),
        ));
        assert!(
            issues
                .iter()
                .any(|i| i.contains("services.api.prepare.link")),
            "{bad}: {issues:?}"
        );
    }
}

#[test]
fn prepare_outputs_are_directories_inside_the_repository_and_hold_its_links() {
    use devstack_config::{OUTPUTS_SINCE, validate::warnings};
    let with =
        |prep: &str| MINIMAL.replace("port = 5000", &format!("port = 5000\nprepare = {prep}"));
    let ok = with(
        r#"{ run = "npm ci", inputs = ["web/package-lock.json"], outputs = ["web/node_modules"], link = ["./web\\node_modules/"], version = "node --version" }"#,
    );
    let mut s = from_str(&ok, Path::new("t")).expect("loads");
    let prep = s.services["api"].prepare.as_ref().unwrap();
    assert_eq!(prep.outputs, ["web/node_modules"]);
    assert_eq!(prep.version.as_deref(), Some("node --version"));
    let w = warnings(&s);
    for key in ["outputs", "version"] {
        assert!(
            w.iter()
                .any(|i| i.path == format!("services.api.prepare.{key}")
                    && i.message.contains(OUTPUTS_SINCE)),
            "{key}: {w:?}"
        );
    }
    s.stack.requires = Some(OUTPUTS_SINCE.into());
    assert!(warnings(&s).is_empty());

    // A link inside an output is fine; without `outputs`, any link is.
    from_str(
        &with(r#"{ run = "npm ci", outputs = ["web"], link = ["web/node_modules"] }"#),
        Path::new("t"),
    )
    .expect("a link inside an output");

    for (prep, expect) in [
        (
            r#"{ run = "npm ci", outputs = ["../node_modules"] }"#,
            "services.api.prepare.outputs",
        ),
        (
            r#"{ run = "npm ci", outputs = ["."] }"#,
            "services.api.prepare.outputs: `.`: a directory below the repository",
        ),
        (
            r#"{ run = "npm ci", outputs = ["{repo}/node_modules"] }"#,
            "services.api.prepare.outputs",
        ),
        (
            r#"{ run = "npm ci", outputs = ["web/node_modules"], link = ["web/node_modules-old"] }"#,
            "services.api.prepare.link: `web/node_modules-old` is not one of `outputs`",
        ),
        (
            r#"{ run = "npm ci", version = "node --version" }"#,
            "services.api.prepare.version: keys the store",
        ),
        (
            r#"{ run = "npm ci", outputs = ["web/node_modules"], version = " " }"#,
            "services.api.prepare.version: is empty",
        ),
    ] {
        let issues = invalid(&with(prep));
        assert!(
            issues.iter().any(|i| i.contains(expect)),
            "{prep}: {issues:?}"
        );
    }
}

#[test]
fn prepare_warm_names_refs_and_needs_outputs_to_warm() {
    use devstack_config::{WARM_SINCE, validate::warnings};
    let with = |warm: &str, outputs: bool| {
        let prep = if outputs {
            "{ run = \"npm ci\", outputs = [\"web/node_modules\"] }"
        } else {
            "{ run = \"npm ci\" }"
        };
        format!(
            "{}\n[prepare]\nwarm = {warm}\n",
            MINIMAL.replace("port = 5000", &format!("port = 5000\nprepare = {prep}"))
        )
    };
    let mut s = from_str(&with("[\"origin/main\"]", true), Path::new("t")).unwrap();
    assert_eq!(s.prepare.warm, ["origin/main"]);
    assert!(
        warnings(&s)
            .iter()
            .any(|i| i.path == "prepare.warm" && i.message.contains(WARM_SINCE))
    );
    s.stack.requires = Some(WARM_SINCE.into());
    assert!(warnings(&s).iter().all(|i| i.path != "prepare.warm"));
    for bad in ["--upload-pack=x", "main..dev", "a b", "HEAD~1", "x:y", " "] {
        let issues = invalid(&with(&format!("[\"{bad}\"]"), true));
        assert!(
            issues.iter().any(|i| i.contains("prepare.warm")),
            "{bad}: {issues:?}"
        );
    }
    let issues = invalid(&with("[\"origin/main\"]", false));
    assert!(
        issues
            .iter()
            .any(|i| i.contains("nothing would be kept to warm")),
        "{issues:?}"
    );
}

#[test]
fn working_directories_and_the_output_stay_inside_the_repository() {
    let issues = invalid(&format!(
        "{MINIMAL}cwd = \"../outside\"\n[services.api.prepare]\nrun = \"npm ci\"\ncwd = \"/abs\"\n[services.api.hot_reload]\nrun = \"dotnet watch run\"\ncwd = \"C:/elsewhere\"\n"
    ));
    for path in [
        "services.api.cwd",
        "services.api.prepare.cwd",
        "services.api.hot_reload.cwd",
    ] {
        assert!(
            has(&issues, path, "inside the repository"),
            "{path}: {issues:?}"
        );
    }
    for output in ["C:/tmp/out", "../out", "{repo}/../out", "/out"] {
        let text = MINIMAL.replace("output = \"bin\"", &format!("output = \"{output}\""));
        let issues = invalid(&text);
        assert!(
            has(&issues, "services.api.output", "inside the repository"),
            "{output}: {issues:?}"
        );
    }
    for output in ["bin", "{repo}/bin", "{repo}", "${OUT_DIR:-bin}"] {
        let text = MINIMAL.replace("output = \"bin\"", &format!("output = \"{output}\""));
        from_str(&text, Path::new("t.toml")).unwrap_or_else(|e| panic!("{output}: {e}"));
    }
}

#[test]
fn a_health_check_needs_time() {
    let issues = invalid(&format!(
        "{MINIMAL}health = {{ http = \"/health\", timeout = \"0s\" }}\n"
    ));
    assert!(
        has(&issues, "services.api.health.timeout", "more than 0"),
        "{issues:?}"
    );
}

#[test]
fn ports_a_first_lane_would_collide_on_are_a_warning() {
    let text = format!(
        "{MINIMAL}[services.web]\nkind = \"devserver\"\nrun = \"serve\"\nport = 5100\n[services.docs]\nkind = \"devserver\"\nrun = \"serve\"\nport = 4200\n"
    );
    let s = from_str(&text, Path::new("t.toml")).unwrap();
    let w: Vec<String> = devstack_config::validate::warnings(&s)
        .iter()
        .map(|i| i.to_string())
        .collect();
    // 5000 and 5100: lane 1. 4200 against 5000 and 5100: lanes 8 and 9, no warning.
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].starts_with("services.web.port") && w[0].contains("lane 1"),
        "{w:?}"
    );
    let other = text.replace("port = 5100", "port = 5150");
    let s = from_str(&other, Path::new("t.toml")).unwrap();
    assert!(devstack_config::validate::warnings(&s).is_empty());
}

#[test]
fn a_lanes_need_is_a_floor_a_profile_raises() {
    let text = format!(
        r#"{MINIMAL}
[lanes.need]
memory = "6G"
disk = "25GiB"
cpus = 2

[profiles.ml.need]
memory = "8G"
gpus = {{ memory = "16G" }}
"#
    );
    let stack = from_str(&text, Path::new("t")).unwrap_or_else(|e| panic!("{e}"));
    let n = stack.lane_need(None);
    assert_eq!(
        (n.cpus, n.memory, n.disk, n.gpus.is_none()),
        (Some(2.0), Some(6 << 30), Some(25 << 30), true)
    );
    let n = stack.lane_need(Some("ml"));
    assert_eq!((n.memory, n.disk), (Some(8 << 30), Some(25 << 30)));
    let g = n.gpus.unwrap();
    assert_eq!((g.count, g.memory), (1, Some(16 << 30)));
    assert_eq!(devstack_config::model::parse_size("512M"), Ok(512 << 20));
    assert_eq!(devstack_config::model::parse_size("1.5k"), Ok(1536));
    assert_eq!(devstack_config::model::parse_size("2048"), Ok(2048));
    assert!(devstack_config::model::parse_size("6 gigs").is_err());

    let bad = invalid(&format!(
        "{MINIMAL}\n[lanes.need]\ncpus = 0\ngpus = {{ count = 0 }}\n"
    ));
    assert!(bad.iter().any(|i| i.contains("lanes.need.cpus")), "{bad:?}");
    assert!(
        bad.iter().any(|i| i.contains("lanes.need.gpus.count")),
        "{bad:?}"
    );
    let err = from_str(
        &format!("{MINIMAL}\n[lanes.need]\nmemory = \"lots\"\n"),
        Path::new("t"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("invalid size"), "{err}");
}

#[test]
fn main_ports_name_started_services_a_lane_runs_itself() {
    let group = |extra: &str| {
        format!(
            "{MINIMAL}\n[compose.ui]\nproject = \"ui-dev\"\nfiles = [\"c.yml\"]\nservices = [\"web\", \"mail\"]\nshared = [\"mail\"]\n{extra}"
        )
    };
    from_str(&group("main_ports = [\"web\"]\n"), Path::new("t")).unwrap();
    let all = invalid(&group("main_ports = [\"nope\", \"mail\"]\n")).join("\n");
    assert!(
        all.contains("`nope` is not in this group's `services`"),
        "{all}"
    );
    assert!(all.contains("`mail` is `shared`"), "{all}");
}

#[test]
fn agent_skill_folders_default_to_claude_code_and_stay_in_the_repository() {
    let stack = from_str(MINIMAL, Path::new("t.toml")).unwrap();
    assert_eq!(stack.agents.skill_dirs(), [".claude/skills"]);

    let both = format!("{MINIMAL}\n[agents]\nskills = [\".claude/skills\", \".agents/skills\"]\n");
    let stack = from_str(&both, Path::new("t.toml")).unwrap();
    assert_eq!(
        stack.agents.skill_dirs(),
        [".claude/skills", ".agents/skills"]
    );

    let bad = format!(
        "{MINIMAL}\n[agents]\nskills = [\"../elsewhere\", \"/abs\", \".agents/skills\", \".agents/skills/\"]\n"
    );
    let issues = invalid(&bad).join("\n");
    assert!(
        issues.contains("`../elsewhere` must be a folder inside"),
        "{issues}"
    );
    assert!(
        issues.contains("`/abs` must be a folder inside"),
        "{issues}"
    );
    assert!(
        issues.contains("`.agents/skills/` is named twice"),
        "{issues}"
    );
    let none = invalid(&format!("{MINIMAL}\n[agents]\nskills = []\n")).join("\n");
    assert!(none.contains("at least one folder"), "{none}");
}

#[test]
fn a_dev_server_with_a_package_json_and_no_prepare_is_a_warning() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("web")).unwrap();
    std::fs::write(repo.path().join("web/package.json"), "{}").unwrap();
    let text = format!(
        "{MINIMAL}[services.web]\nkind = \"devserver\"\ncwd = \"web\"\nrun = \"npm run dev\"\nport = 5173\n"
    );
    let warnings = |text: &str| -> Vec<String> {
        let s = from_str(text, Path::new("t.toml")).unwrap();
        devstack_config::validate::repo_warnings(&s, repo.path())
            .iter()
            .map(|i| i.to_string())
            .collect()
    };
    let w = warnings(&text);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].starts_with("services.web:")
            && w[0].contains("node_modules")
            && w[0].contains("web/package-lock.json"),
        "{w:?}"
    );
    // With `prepare`, in the main tree only, or without a package.json: none.
    let prepared = text.replace(
        "port = 5173\n",
        "port = 5173\nprepare = { run = \"npm ci\" }\n",
    );
    assert!(warnings(&prepared).is_empty());
    let main_only = text.replace("port = 5173\n", "port = 5173\nlanes = false\n");
    assert!(warnings(&main_only).is_empty());
    assert!(warnings(&text.replace("cwd = \"web\"", "cwd = \"api\"")).is_empty());
}

#[test]
fn an_announced_port_names_a_started_service_and_gets_a_variable_of_its_own() {
    use devstack_config::model::AnnouncedPort;
    let group = |entries: &str| {
        format!(
            "{MINIMAL}\n[compose.media]\nproject = \"media-dev\"\nfiles = [\"c.yml\"]\nservices = [\"media\", \"turn-relay\", \"turn_relay\", \"mail\"]\nshared = [\"mail\"]\nannounced_ports = [{entries}]\n"
        )
    };
    let s = from_str(
        &group(r#""media:7881", "media:7882/udp", "media:7882", "turn-relay:3478", "mail:1025""#),
        Path::new("t"),
    )
    .unwrap();
    let g = &s.compose["media"];
    assert_eq!(
        g.announced()[1],
        AnnouncedPort {
            service: "media".into(),
            port: 7882,
            udp: true
        }
    );
    assert_eq!(g.announced()[3].variable(), "DEVSTACK_PORT_TURN_RELAY_3478");
    // The main tree: the files' ports. A lane: moved, but not a `shared`
    // service's, which the main tree runs. TCP and UDP share a variable.
    let main = g.port_variables(0, &[]);
    assert_eq!(main.len(), 4, "{main:?}");
    assert_eq!(main["DEVSTACK_PORT_MEDIA_7881"], "7881");
    let lane = g.port_variables(100, &g.shared);
    assert_eq!(lane["DEVSTACK_PORT_MEDIA_7881"], "7981");
    assert_eq!(lane["DEVSTACK_PORT_MEDIA_7882"], "7982");
    assert_eq!(lane["DEVSTACK_PORT_MAIL_1025"], "1025");
    assert!(g.port_variables(65000, &[]).is_empty(), "beyond 65535");

    let all = invalid(&group(
        r#""media", "media:7881/tcp", "media:7881-7890", "media:0", "nope:1", "media:7881", "media:7881", "turn-relay:3478", "turn_relay:3478""#,
    ))
    .join("\n");
    for want in [
        "announced_ports[0]: `media`: an entry is `service:port`",
        "announced_ports[1]: `media:7881/tcp`: an entry is",
        "announced_ports[2]: `media:7881-7890`: a port range cannot be announced",
        "announced_ports[3]: `media:0`: `0` is no port",
        "announced_ports[4]: `nope` is not in this group's `services`",
        "announced_ports[6]: `media:7881` is named twice",
        "announced_ports[8]: `turn_relay:3478` and `turn-relay:3478` would both be told in `DEVSTACK_PORT_TURN_RELAY_3478`",
    ] {
        assert!(all.contains(want), "{want}\n{all}");
    }
}

#[test]
fn announced_ports_want_requires_at_their_first_devstack() {
    use devstack_config::{ANNOUNCED_SINCE, validate::warnings};
    let text = MINIMAL.replace(
        "services = [\"db\", \"queue\"]\n",
        "services = [\"db\", \"queue\"]\nannounced_ports = [\"db:5432\"]\n",
    );
    let mut s = from_str(&text, Path::new("t")).unwrap();
    let w = warnings(&s);
    assert!(
        w.iter()
            .any(|i| i.path == "compose.app.announced_ports" && i.message.contains(ANNOUNCED_SINCE)),
        "{w:?}"
    );
    s.stack.requires = Some(ANNOUNCED_SINCE.into());
    assert!(warnings(&s).is_empty());
}

#[test]
fn the_files_text_is_held_against_the_announced_ports() {
    use devstack_config::validate::announced_warnings;
    let text = MINIMAL.replace(
        "services = [\"db\", \"queue\"]\n",
        "services = [\"db\", \"queue\"]\nannounced_ports = [\"db:5432\", \"queue:5672\", \"queue:5673\"]\n",
    );
    let s = from_str(&text, Path::new("t")).unwrap();
    let compose = r#"
services:
  db:
    ports: ["5432:5432"]
    command: ["postgres", "-p", "${DEVSTACK_PORT_DB_5432:-5432}"]
  queue:
    environment:
      LISTEN: ${DEVSTACK_PORT_QUEUE_5672}
      ADVERTISED: $DEVSTACK_PORT_QUEUE_5672
      TYPO: ${DEVSTACK_PORT_QEUE_5672:-5672}
      SHELL: sh -c "echo $${DEVSTACK_PORT_QUEUE_5672}"
      MY_DEVSTACK_PORT_X: "1"
"#
    .to_string();
    let env = "DEVSTACK_PORT_QUEUE_5673=5673\n".to_string();
    let w: Vec<String> = announced_warnings(
        "app",
        &s.compose["app"],
        &[("compose.yml", compose), (".env", env)],
    )
    .iter()
    .map(|i| i.to_string())
    .collect();
    let all = w.join("\n");
    assert!(
        all.contains("`compose.yml` uses `DEVSTACK_PORT_QEUE_5672`, which no entry"),
        "{all}"
    );
    assert!(
        all.contains("`compose.yml` reads `DEVSTACK_PORT_QUEUE_5672` without a default")
            && all.contains("${DEVSTACK_PORT_QUEUE_5672:-5672}"),
        "{all}"
    );
    // Set in an env file, used nowhere: the server would not listen there.
    assert!(
        all.contains("uses `DEVSTACK_PORT_QUEUE_5673` (`queue:5673`)"),
        "{all}"
    );
    assert!(!all.contains("DEVSTACK_PORT_DB_5432"), "{all}");
    assert!(!all.contains("DEVSTACK_PORT_X"), "{all}");
    assert_eq!(w.len(), 3, "each once: {all}");
}

#[test]
fn every_compose_call_is_told_the_lane() {
    use devstack_config::model::lane_variables;
    // As `{lane}` and `{lane_suffix}` have it (ADR-0064).
    for (lane, name, suffix) in [
        ("", "main", ""),
        ("main", "main", ""),
        ("feat-x", "feat-x", "_feat-x"),
    ] {
        let v = lane_variables(lane);
        assert_eq!(v["DEVSTACK_LANE"], name, "lane `{lane}`");
        assert_eq!(v["DEVSTACK_LANE_SUFFIX"], suffix, "lane `{lane}`");
        assert_eq!(v.len(), 2);
    }
    // With the announced ports' variables, in one environment.
    let text = MINIMAL.replace(
        "services = [\"db\", \"queue\"]\n",
        "services = [\"db\", \"queue\"]\nannounced_ports = [\"db:5432\"]\n",
    );
    let s = from_str(&text, Path::new("t")).unwrap();
    let v = s.compose["app"].variables("feat-x", 100, &[]);
    assert_eq!(v["DEVSTACK_PORT_DB_5432"], "5532");
    assert_eq!(v["DEVSTACK_LANE_SUFFIX"], "_feat-x");
    assert_eq!(v.len(), 3, "{v:?}");
    // Without `announced_ports` the lane is told all the same.
    let s = from_str(MINIMAL, Path::new("t")).unwrap();
    assert_eq!(s.compose["app"].variables("", 0, &[]), lane_variables(""));
}

#[test]
fn the_files_read_the_lane_with_a_default() {
    use devstack_config::validate::lane_warnings;
    let compose = r#"
services:
  scheduler:
    environment:
      - REFRESH_COOKIE=app_refresh${DEVSTACK_LANE_SUFFIX:-}
      - SESSION=app_${DEVSTACK_LANE}
      - TAG=$DEVSTACK_LANE_SUFFIX
      - OTHER=${DEVSTACK_LANE:-main}-${DEVSTACK_LANE_SUFIX:-}
      - SHELL=sh -c "echo $${DEVSTACK_LANE}"
      - ${DEVSTACK_LANES:-x}
      - MY_DEVSTACK_LANE=1
"#
    .to_string();
    let env = "DEVSTACK_LANE_OWN=1\n".to_string();
    let w: Vec<String> = lane_warnings("app", &[("compose.yml", compose), (".env", env)])
        .iter()
        .map(|i| i.to_string())
        .collect();
    let all = w.join("\n");
    assert!(
        all.contains("`compose.yml` reads `DEVSTACK_LANE` without a default")
            && all.contains("${DEVSTACK_LANE:-main}"),
        "{all}"
    );
    assert!(
        all.contains("`compose.yml` reads `DEVSTACK_LANE_SUFFIX` without a default")
            && all.contains("${DEVSTACK_LANE_SUFFIX:-}"),
        "{all}"
    );
    assert!(
        all.contains("`compose.yml` uses `DEVSTACK_LANE_SUFIX`, which devstack does not set"),
        "{all}"
    );
    // Set in an env file, another name, a longer name: none of devstack's.
    assert!(!all.contains("DEVSTACK_LANE_OWN"), "{all}");
    assert!(!all.contains("DEVSTACK_LANES"), "{all}");
    assert_eq!(w.len(), 3, "each once: {all}");
    assert!(w.iter().all(|i| i.starts_with("compose.app")), "{all}");
}

#[test]
fn a_services_stop_takes_the_placeholders_of_its_run() {
    use devstack_config::{STOP_SINCE, validate::warnings};
    let text = MINIMAL.replace(
        "port = 5000\n",
        "port = 5000\nstop = \"docker rm -f api{lane_suffix}-{port}\"\n",
    );
    let mut s = from_str(&text, Path::new("t")).unwrap();
    assert_eq!(
        s.services["api"].stop.as_deref(),
        Some("docker rm -f api{lane_suffix}-{port}")
    );
    let w = warnings(&s);
    assert!(
        w.iter()
            .any(|i| i.path == "services.api.stop" && i.message.contains(STOP_SINCE)),
        "{w:?}"
    );
    s.stack.requires = Some(STOP_SINCE.into());
    assert!(warnings(&s).is_empty());

    // An unknown placeholder, an empty command and a static service's stop.
    let all = invalid(&MINIMAL.replace("port = 5000\n", "port = 5000\nstop = \"rm {nope}\"\n"))
        .join("\n");
    assert!(all.contains("services.api.stop"), "{all}");
    let all = invalid(&MINIMAL.replace("port = 5000\n", "port = 5000\nstop = \" \"\n")).join("\n");
    assert!(all.contains("services.api.stop: is empty"), "{all}");
    let all = invalid(&format!(
        "{MINIMAL}[services.site]\nkind = \"static\"\nbuild = \"npm run build\"\noutput = \"dist\"\nport = 4300\nstop = \"true\"\n"
    ))
    .join("\n");
    assert!(
        all.contains("services.site.stop: kind `static` has no `stop`"),
        "{all}"
    );
}
