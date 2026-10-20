//! Profiles (ADR-0041): an overlay chosen when a tree starts, checked like
//! the stack file it makes.

use devstack_config::model::{DataMode, Kind};
use std::path::Path;

const BASE: &str = r#"
version = 1
[stack]
name = "shop"

[services.api]
build  = "dotnet build"
output = "bin"
run    = "dotnet {artifact}/Api.dll"
port   = 5000
env    = { Mode = "dev" }

[services.web]
kind = "devserver"
run  = "npx ng serve --port {port}"
port = 4200

[services.mailer]
run = "mailer"

[commands.e2e]
description = "the end-to-end suite"
steps = [{ exec = "npx playwright test" }]
"#;

fn load(extra: &str) -> Result<devstack_config::Stack, devstack_config::LoadError> {
    devstack_config::from_str(&format!("{BASE}{extra}"), Path::new("devstack.toml"))
}

#[test]
fn a_profile_changes_how_the_stack_runs() {
    let s = load(
        r#"
[profiles.acceptance]
description = "an acceptance run"
workers   = true
data      = "fresh"
env       = { BASE_URL = "{service.web.url}", Mode = "e2e" }
autostart = ["api", "web"]
command   = "e2e"

[profiles.acceptance.services.web]
kind   = "static"
build  = "npm run build"
output = "dist/web/browser"
env    = { NG_ENV = "e2e" }
"#,
    )
    .unwrap();
    // Without it, the stack is as written.
    assert_eq!(s.services["web"].kind, Kind::Devserver);
    assert!(s.profile.is_none());

    let p = s.with_profile("acceptance").unwrap();
    let web = &p.services["web"];
    assert_eq!(web.kind, Kind::Static);
    assert_eq!(web.run, "", "a static service has no run");
    assert_eq!(web.output.as_deref(), Some("dist/web/browser"));
    assert_eq!(web.env["NG_ENV"], "e2e");
    assert_eq!(web.env["BASE_URL"], "{service.web.url}");
    assert_eq!(
        p.services["api"].env["Mode"], "e2e",
        "the profile's env over the service's"
    );
    assert!(p.services["api"].autostart && !p.services["mailer"].autostart);
    let a = p.profile.unwrap();
    assert_eq!(
        (a.name.as_str(), a.workers, a.data, a.command.as_deref()),
        ("acceptance", Some(true), DataMode::Fresh, Some("e2e"))
    );
    assert!(
        p.profiles.contains_key("acceptance"),
        "what else there is stays known"
    );
}

#[test]
fn a_profile_is_checked_like_the_stack_file() {
    // A service or a command that is not there.
    let e = load(
        r#"
[profiles.x.services.nope]
kind = "static"
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("profiles.x.services.nope") && e.contains("no service"),
        "{e}"
    );
    let e = load("[profiles.x]\ncommand = \"suite\"\nautostart = [\"ghost\"]\n")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("profiles.x.command") && e.contains("profiles.x.autostart[0]"),
        "{e}"
    );
    // What it makes must hold: a static service needs its output.
    let e = load(
        r#"
[profiles.x.services.web]
kind  = "static"
build = "npm run build"
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("profiles.x (services.web)") && e.contains("static"),
        "{e}"
    );
    // An unknown key in a profile is an error, not a silent no-op.
    let e = load("[profiles.x]\nworker = true\n")
        .unwrap_err()
        .to_string();
    assert!(e.contains("worker"), "{e}");
    // And so is a profile that is not there.
    let s = load("").unwrap();
    let e = s.with_profile("acceptance").unwrap_err();
    assert!(e[0].message.contains("has none"), "{e:?}");
}

/// A profile that makes a dev server a `static` build (ADR-0041's own
/// example) over one that says how it rebuilds (ADR-0042).
#[test]
fn another_kind_drops_what_only_the_old_one_takes() {
    let text = BASE.replace(
        "port = 4200\n",
        "port = 4200\nrebuild = \"angular\"\nrestart_on = [\"angular.json\"]\nproxy = true\n",
    );
    let s = devstack_config::from_str(
        &format!(
            "{text}\n[profiles.acc.services.web]\nkind = \"static\"\nbuild = \"npm run build\"\noutput = \"dist/web/browser\"\n"
        ),
        Path::new("devstack.toml"),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(s.services["web"].rebuild.is_some());
    let p = s.with_profile("acc").unwrap();
    let web = &p.services["web"];
    assert_eq!(web.kind, Kind::Static);
    assert!(web.rebuild.is_none() && web.restart_on.is_empty() && web.run.is_empty());
    assert_eq!(web.proxy, Some(true));

    // And the other way: a built service made a dev server loses its build.
    let p = load(
        "[profiles.dev.services.api]\nkind = \"devserver\"\nrun = \"dotnet watch run --urls http://localhost:{port}\"\n",
    )
    .unwrap()
    .with_profile("dev")
    .unwrap();
    assert!(p.services["api"].build.is_none() && p.services["api"].output.is_none());
}

#[test]
fn a_profiles_env_takes_the_placeholders_of_a_command() {
    let e = load("[profiles.x]\nenv = { P = \"{port}\" }\n")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("profiles.x.env.P") && e.contains("not available in a command"),
        "{e}"
    );
    load("[profiles.x]\nenv = { U = \"{service.api.url}\", L = \"{lane}\" }\n").unwrap();
}
