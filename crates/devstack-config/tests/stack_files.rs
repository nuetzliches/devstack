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
        has(
            &issues,
            "commands.c.steps[0].exec",
            "`{compose.app.db.5432}`: is not available"
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
