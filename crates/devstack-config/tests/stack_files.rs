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
