//! `devstack init`: a first `devstack.toml` from what the repository shows.
//!
//! A draft, not a finished file: what can be read off the repository (Compose
//! files and their services, .NET web projects with their launch ports, Node
//! dev servers, Go main packages, Python web apps (Django, FastAPI, Flask),
//! Spring Boot apps) is filled in, every guess is marked `# TODO:`. A person or an
//! agent works through the TODOs with the stack file reference and
//! `devstack check`. Writes nothing when a stack file exists.

use anyhow::{Result, bail};
use serde::Serialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// What `init` found and wrote (also `--json` for the hub).
#[derive(Debug, Serialize)]
pub struct Draft {
    pub file: PathBuf,
    pub compose_files: Vec<String>,
    pub services: Vec<String>,
    pub todos: usize,
    pub text: String,
}

struct Svc {
    name: String,
    body: String,
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".devstack",
    "node_modules",
    "bin",
    "obj",
    "target",
    "dist",
    "build",
    ".angular",
    ".vs",
    ".idea",
    "vendor",
    ".next",
    "coverage",
];

/// `packages/` of NuGet's `packages.config` era (restored packages, named
/// `<Id>.<version>`), not the `packages/` of a JavaScript monorepo.
fn is_nuget_packages(dir: &Path) -> bool {
    if dir.join("repositories.config").is_file() {
        return true;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        let n = e.file_name().to_string_lossy().to_string();
        // `Newtonsoft.Json.13.0.1`: ends in a version.
        let mut parts = n.rsplit('.');
        e.path().is_dir()
            && parts
                .next()
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            && parts
                .next()
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Walk the repository a few levels deep, without generated directories.
fn files(repo: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(repo.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if depth < 5
                    && !SKIP_DIRS.contains(&name.as_str())
                    && !name.starts_with('.')
                    && !(name == "packages" && is_nuget_packages(&p))
                {
                    stack.push((p, depth + 1));
                }
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn rel(repo: &Path, p: &Path) -> String {
    p.strip_prefix(repo)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// `My.Api` -> `my-api`. A name without any ASCII letter or digit (`Ünü`,
/// `日本`) becomes `app`: a stack file name must not be empty.
fn kebab(s: &str) -> String {
    let out = kebab_raw(s);
    if out.is_empty() { "app".into() } else { out }
}

/// `kebab` that may be empty, for a caller with a fallback of its own.
fn kebab_raw(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Every `from` that stands as a number of its own (not part of `50001` or
/// `1.5000`) becomes `to`.
fn replace_number(text: &str, from: u16, to: u16) -> String {
    let from = from.to_string();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i..].starts_with(&from) {
            let end = i + from.len();
            let before = i.checked_sub(1).map(|j| bytes[j]);
            let after = bytes.get(end).copied();
            let digit_or_dot = |b: Option<u8>| b.is_some_and(|b| b.is_ascii_digit() || b == b'.');
            if !digit_or_dot(before) && !after.is_some_and(|b| b.is_ascii_digit()) {
                out.push_str(&to.to_string());
                i = end;
                continue;
            }
        }
        let c = text[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let i = text.find(open)? + open.len();
    let j = text[i..].find(close)? + i;
    Some(text[i..j].trim())
}

fn compose_files(repo: &Path, all: &[PathBuf]) -> Vec<String> {
    let names = [
        "compose.yml",
        "compose.yaml",
        "docker-compose.yml",
        "docker-compose.yaml",
    ];
    let mut found: Vec<String> = all
        .iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| names.contains(&n.to_string_lossy().to_lowercase().as_str()))
        })
        .map(|p| rel(repo, p))
        .collect();
    // The likeliest main file first: not a production, CI or test variant, and
    // as close to the root as possible.
    let rank = |f: &String| {
        let l = f.to_lowercase();
        let side = [
            "prod", "staging", "_runner", "e2e", "test", "loadgen", "ansible",
        ]
        .iter()
        .any(|w| l.contains(w));
        (side, f.matches('/').count(), f.clone())
    };
    found.sort_by_key(rank);
    found
}

/// The services of a Compose file, asked from Docker (no YAML parser here).
fn compose_services(repo: &Path, file: &str) -> Option<Vec<String>> {
    // A hanging Docker CLI must not hang `init` (nor the hub that runs it).
    let out = crate::doctor::output_within(
        std::process::Command::new("docker")
            .args(["compose", "-f", file, "config", "--services"])
            .current_dir(repo),
        std::time::Duration::from_secs(30),
    )?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn dotnet(repo: &Path, csproj: &Path, port: u16) -> Option<Svc> {
    let text = std::fs::read_to_string(csproj).ok()?;
    let stem = csproj.file_stem()?.to_string_lossy().to_string();
    let lower = stem.to_lowercase();
    if lower.contains("test") || lower.contains("benchmark") {
        return None;
    }
    let web = text.contains("Microsoft.NET.Sdk.Web");
    let exe = text.contains("<OutputType>Exe</OutputType>");
    if !web && !exe {
        return None; // a library
    }
    let dir = csproj.parent()?;
    let tfm = between(&text, "<TargetFramework>", "</TargetFramework>")
        .map(str::to_string)
        .unwrap_or_else(|| "net10.0".into());
    let assembly = between(&text, "<AssemblyName>", "</AssemblyName>")
        .map(str::to_string)
        .unwrap_or_else(|| stem.clone());
    let launch_port = std::fs::read_to_string(dir.join("Properties/launchSettings.json"))
        .ok()
        .and_then(|t| {
            let i = t.find("\"applicationUrl\"")?;
            let rest = &t[i..];
            let http = rest
                .find("http://localhost:")
                .or_else(|| rest.find("https://localhost:"))?;
            let digits: String = rest[http..]
                .split(':')
                .nth(2)?
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse::<u16>().ok()
        });
    let d = rel(repo, dir);
    let p = rel(repo, csproj);
    let mut b = String::new();
    let _ = writeln!(b, "build  = \"dotnet build {p} -c Debug -nologo -v q\"");
    let _ = writeln!(b, "output = \"{d}/bin/Debug/{tfm}\"");
    let _ = writeln!(b, "run    = \"dotnet {{artifact}}/{assembly}.dll\"");
    let _ = writeln!(b, "watch  = [\"{d}/**\"]");
    if web {
        match launch_port {
            Some(lp) => {
                let _ = writeln!(b, "port   = {lp}   # from Properties/launchSettings.json");
            }
            None => {
                let _ = writeln!(b, "port   = {port}   # TODO: the port others reach it on");
            }
        }
        let _ = writeln!(
            b,
            "health = {{ http = \"/health\", timeout = \"120s\" }}   # TODO: a readiness endpoint (checks the database), or remove"
        );
        let _ = writeln!(
            b,
            "# TODO: environment from a Compose service: compose = {{ group = \"app\", service = \"...\" }}"
        );
        let _ = writeln!(b, "\n[services.{}.env]", kebab(&stem));
        let _ = writeln!(b, "ASPNETCORE_URLS = \"http://127.0.0.1:{{port}}\"");
        let _ = writeln!(b, "ASPNETCORE_ENVIRONMENT = \"Development\"");
        let _ = writeln!(
            b,
            "\n# `devstack hot-reload {} on` for a debugging session (plan §3.4a)",
            kebab(&stem)
        );
        let _ = writeln!(b, "[services.{}.hot_reload]", kebab(&stem));
        let _ = writeln!(
            b,
            "run = \"dotnet watch run --project {p} --non-interactive\""
        );
    } else {
        let _ = writeln!(
            b,
            "# TODO: a console app: kind = \"job\" if it runs to completion (a migrator), otherwise keep it a service"
        );
    }
    Some(Svc {
        name: kebab(&stem),
        body: b,
    })
}

fn node(repo: &Path, pkg: &Path) -> Option<Svc> {
    let text = std::fs::read_to_string(pkg).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let scripts = v["scripts"].as_object()?;
    let script = ["dev", "start", "serve"]
        .into_iter()
        .find(|s| scripts.contains_key(*s))?;
    let deps =
        |n: &str| v["dependencies"].get(n).is_some() || v["devDependencies"].get(n).is_some();
    let (port, what) = if deps("@angular/core") {
        (4200, "Angular dev server")
    } else if deps("vite") {
        (5173, "Vite dev server")
    } else if deps("next") {
        (3000, "Next.js dev server")
    } else if deps("nuxt") {
        (3000, "Nuxt dev server")
    } else if deps("react-scripts") {
        (3000, "Create React App dev server")
    } else {
        (3000, "dev server, port unknown")
    };
    let dir = pkg.parent()?;
    let d = rel(repo, dir);
    let name = v["name"]
        .as_str()
        .map(|n| kebab_raw(n.rsplit('/').next().unwrap_or(n)))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            if d.is_empty() {
                "web".into()
            } else {
                kebab(&d)
            }
        });
    let mut b = String::new();
    let _ = writeln!(
        b,
        "kind   = \"devserver\"   # {what}: builds and reloads itself"
    );
    if !d.is_empty() {
        let _ = writeln!(b, "cwd    = \"{d}\"");
    }
    let _ = writeln!(b, "run    = \"npm run {script}\"");
    let _ = writeln!(
        b,
        "port   = {port}   # TODO: check; a dev server keeps its own fixed port"
    );
    let _ = writeln!(
        b,
        "health = {{ http = \"/\", timeout = \"300s\" }}   # add insecure_tls = true if it serves HTTPS"
    );
    let _ = writeln!(
        b,
        "restart_on = [\"{}package.json\"]   # TODO: config files hot reload ignores",
        if d.is_empty() {
            String::new()
        } else {
            format!("{d}/")
        }
    );
    Some(Svc { name, body: b })
}

/// A Go `main` package: built with `go build`, the port through `PORT`.
fn golang(repo: &Path, main_go: &Path, module_dir: &Path, port: u16) -> Option<Svc> {
    let text = std::fs::read_to_string(main_go).ok()?;
    if !text.lines().any(|l| l.trim() == "package main") || !text.contains("func main()") {
        return None;
    }
    let dir = main_go.parent()?;
    let lower = rel(repo, dir).to_lowercase();
    if lower.contains("test") || lower.contains("example") || lower.contains("tools") {
        return None;
    }
    let name = kebab(&dir.file_name()?.to_string_lossy());
    let module = rel(repo, module_dir);
    let pkg = dir
        .strip_prefix(module_dir)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let pkg = if pkg.is_empty() {
        ".".to_string()
    } else {
        format!("./{pkg}")
    };
    let out = if module.is_empty() {
        format!("bin/{name}")
    } else {
        format!("{module}/bin/{name}")
    };
    let watch = if module.is_empty() {
        String::new()
    } else {
        format!("{module}/")
    };
    let mut b = String::new();
    if module.is_empty() {
        let _ = writeln!(b, "build  = \"go build -o bin/{name}/ {pkg}\"");
    } else {
        let _ = writeln!(
            b,
            "build  = \"cd {module} && go build -o bin/{name}/ {pkg}\""
        );
    }
    let _ = writeln!(b, "output = \"{out}\"");
    let _ = writeln!(b, "run    = \"{{artifact}}/{name}\"");
    let _ = writeln!(
        b,
        "watch  = [\"{watch}**/*.go\", \"{watch}go.mod\", \"{watch}go.sum\"]"
    );
    let _ = writeln!(b, "port   = {port}   # TODO: the port others reach it on");
    let _ = writeln!(
        b,
        "health = {{ http = \"/health\", timeout = \"60s\" }}   # TODO: a readiness endpoint, or remove (a worker without HTTP)"
    );
    let _ = writeln!(b, "\n[services.{name}.env]");
    let _ = writeln!(
        b,
        "PORT = \"{{port}}\"   # TODO: the variable or flag your program reads its listen port from"
    );
    Some(Svc { name, body: b })
}

/// A Python web app: its framework's dev server reloads by itself.
fn python(repo: &Path, dir: &Path, all: &[PathBuf]) -> Option<Svc> {
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .unwrap_or_default()
            .to_lowercase()
    };
    let deps = format!(
        "{}{}{}",
        read("pyproject.toml"),
        read("requirements.txt"),
        read("Pipfile")
    );
    let d = rel(repo, dir);
    let name = if d.is_empty() {
        kebab(&repo.file_name()?.to_string_lossy())
    } else {
        kebab(&d)
    };
    let mut b = String::new();
    let _ = writeln!(b, "kind   = \"devserver\"   # reloads itself on a change");
    if !d.is_empty() {
        let _ = writeln!(b, "cwd    = \"{d}\"");
    }
    if dir.join("manage.py").is_file() {
        let _ = writeln!(
            b,
            "run    = \"python manage.py runserver 127.0.0.1:8000\"   # TODO: the virtualenv's python"
        );
        let _ = writeln!(b, "port   = 8000");
        let _ = writeln!(
            b,
            "health = {{ http = \"/\", timeout = \"60s\" }}   # TODO: a readiness endpoint"
        );
        let _ = writeln!(
            b,
            "restart_on = [\"{}requirements.txt\"]",
            if d.is_empty() {
                String::new()
            } else {
                format!("{d}/")
            }
        );
    } else if deps.contains("fastapi") {
        let app = all
            .iter()
            .filter(|p| p.starts_with(dir) && p.extension().is_some_and(|e| e == "py"))
            .find(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains("FastAPI(")))
            .and_then(|p| {
                p.strip_prefix(dir).ok().map(|r| {
                    r.with_extension("")
                        .to_string_lossy()
                        .replace(['/', '\\'], ".")
                })
            })
            .unwrap_or_else(|| "main".into());
        let _ = writeln!(
            b,
            "run    = \"uvicorn {app}:app --reload --host 127.0.0.1 --port 8000\"   # TODO: check the module and the app object"
        );
        let _ = writeln!(b, "port   = 8000");
        let _ = writeln!(
            b,
            "health = {{ http = \"/docs\", timeout = \"60s\" }}   # TODO: a readiness endpoint"
        );
    } else if deps.contains("flask") {
        let _ = writeln!(
            b,
            "run    = \"flask --app app run --debug --host 127.0.0.1 --port 5000\"   # TODO: the --app module"
        );
        let _ = writeln!(b, "port   = 5000");
        let _ = writeln!(
            b,
            "health = {{ http = \"/\", timeout = \"60s\" }}   # TODO: a readiness endpoint"
        );
    } else {
        return None;
    }
    Some(Svc { name, body: b })
}

/// A Spring Boot app through Maven or Gradle: `spring-boot:run` / `bootRun`
/// build and run it, devtools restarts it.
fn java(repo: &Path, build_file: &Path) -> Option<Svc> {
    let text = std::fs::read_to_string(build_file).ok()?;
    if !text.contains("spring-boot") {
        return None;
    }
    let dir = build_file.parent()?;
    let maven = build_file.file_name()? == "pom.xml";
    let wrapper = if maven {
        dir.join("mvnw").is_file()
    } else {
        dir.join("gradlew").is_file()
    };
    let cmd = match (maven, wrapper) {
        (true, true) => "./mvnw spring-boot:run",
        (true, false) => "mvn spring-boot:run",
        (false, true) => "./gradlew bootRun",
        (false, false) => "gradle bootRun",
    };
    let d = rel(repo, dir);
    let name = between(&text, "<artifactId>", "</artifactId>")
        .filter(|_| maven)
        .map(kebab_raw)
        .or_else(|| dir.file_name().map(|n| kebab_raw(&n.to_string_lossy())))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "app".into());
    let actuator = text.contains("spring-boot-starter-actuator");
    let mut b = String::new();
    let _ = writeln!(
        b,
        "kind   = \"devserver\"   # {cmd} builds and runs it; devtools restarts it on a change"
    );
    if !d.is_empty() {
        let _ = writeln!(b, "cwd    = \"{d}\"");
    }
    let _ = writeln!(b, "run    = \"{cmd}\"");
    let _ = writeln!(b, "port   = 8080   # TODO: server.port");
    if actuator {
        let _ = writeln!(
            b,
            "health = {{ http = \"/actuator/health/readiness\", timeout = \"300s\" }}"
        );
    } else {
        let _ = writeln!(
            b,
            "health = {{ http = \"/\", timeout = \"300s\" }}   # TODO: add spring-boot-starter-actuator for /actuator/health/readiness"
        );
    }
    Some(Svc { name, body: b })
}

pub fn draft(repo: &Path) -> Result<Draft> {
    let file = repo.join("devstack.toml");
    let all = files(repo);
    let compose = compose_files(repo, &all);
    let mut svcs: Vec<Svc> = Vec::new();
    let mut next_port = 5080;
    for p in all
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "csproj"))
    {
        if let Some(s) = dotnet(repo, p, next_port) {
            next_port += 1;
            svcs.push(s);
        }
    }
    for p in all
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "package.json"))
    {
        if let Some(s) = node(repo, p) {
            svcs.push(s);
        }
    }
    let in_skip = |p: &Path| {
        p.components()
            .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
    };
    // Go: every main package below a go.mod.
    let modules: Vec<PathBuf> = all
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "go.mod"))
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .collect();
    for p in all.iter().filter(|p| {
        p.extension().is_some_and(|e| e == "go") && !p.to_string_lossy().ends_with("_test.go")
    }) {
        let Some(module) = modules
            .iter()
            .filter(|m| p.starts_with(m))
            .max_by_key(|m| m.components().count())
        else {
            continue;
        };
        if svcs.iter().any(|s| {
            s.body.contains(&format!(
                "{{artifact}}/{}\"",
                p.parent()
                    .and_then(|d| d.file_name())
                    .map(|n| kebab(&n.to_string_lossy()))
                    .unwrap_or_default()
            ))
        }) {
            continue; // one service per package
        }
        if let Some(s) = golang(repo, p, module, next_port) {
            next_port += 1;
            svcs.push(s);
        }
    }
    // Python: a directory with a project file.
    let mut py_dirs: Vec<PathBuf> = all
        .iter()
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                n == "pyproject.toml" || n == "requirements.txt" || n == "manage.py"
            })
        })
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .filter(|d| !in_skip(d))
        .collect();
    py_dirs.sort();
    py_dirs.dedup();
    for d in &py_dirs {
        if let Some(s) = python(repo, d, &all) {
            svcs.push(s);
        }
    }
    // Java: Spring Boot through Maven or Gradle.
    for p in all.iter().filter(|p| {
        p.file_name()
            .is_some_and(|n| n == "pom.xml" || n == "build.gradle" || n == "build.gradle.kts")
    }) {
        if let Some(s) = java(repo, p) {
            svcs.push(s);
        }
    }
    // Ports must be unique: a second app on a framework's default port moves
    // up, in its command too.
    let mut used = std::collections::BTreeSet::new();
    for s in &mut svcs {
        let Some(port) = s
            .body
            .lines()
            .find_map(|l| l.strip_prefix("port   = "))
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse::<u16>().ok())
        else {
            continue;
        };
        let mut free = port;
        while !used.insert(free) {
            free += 1;
        }
        if free != port {
            s.body = replace_number(&s.body, port, free);
            s.body = s.body.replacen(
                &format!("port   = {free}"),
                &format!("port   = {free}   # TODO: moved from {port}, which another service uses"),
                1,
            );
        }
    }
    // Names must be unique.
    let mut seen = std::collections::BTreeSet::new();
    for s in &mut svcs {
        let mut n = s.name.clone();
        let mut i = 2;
        while !seen.insert(n.clone()) {
            n = format!("{}-{i}", s.name);
            i += 1;
        }
        if n != s.name {
            for sub in ["env", "hot_reload"] {
                s.body = s.body.replace(
                    &format!("[services.{}.{sub}]", s.name),
                    &format!("[services.{n}.{sub}]"),
                );
            }
            s.name = n;
        }
    }

    let stack_name = kebab(
        &repo
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "stack".into()),
    );
    let mut t = String::new();
    let _ = writeln!(
        t,
        "# devstack stack file: a DRAFT from `devstack init` (devstack {}).",
        devstack_config::VERSION
    );
    let _ = writeln!(
        t,
        "# Work through every TODO, then `devstack check` and `devstack doctor`."
    );
    let _ = writeln!(
        t,
        "# Reference: https://github.com/nuetzliches/devstack/blob/v{}/docs/stack-file.md\n",
        devstack_config::VERSION
    );
    let _ = writeln!(
        t,
        "version = 1\n\n[stack]\nname     = \"{stack_name}\"\nrequires = \"{}\"\n",
        devstack_config::VERSION
    );

    if let Some(first) = compose.first() {
        let services = compose_services(repo, first);
        let _ = writeln!(
            t,
            "# Containers from your Compose files (deployment file first, a dev overlay last)."
        );
        let _ = writeln!(t, "[compose.app]");
        let _ = writeln!(t, "project  = \"{stack_name}-dev\"");
        let _ = writeln!(
            t,
            "files    = [\"{first}\"]   # TODO: add a dev overlay (127.0.0.1 ports, dev volumes)"
        );
        match services {
            Some(list) if !list.is_empty() => {
                let quoted: Vec<String> = list.iter().map(|s| format!("\"{s}\"")).collect();
                let _ = writeln!(
                    t,
                    "services = [{}]   # TODO: keep the infrastructure; services you run natively below do not belong here",
                    quoted.join(", ")
                );
            }
            _ => {
                let _ = writeln!(
                    t,
                    "services = []   # TODO: the containers to start (Docker could not list them)"
                );
            }
        }
        if compose.len() > 1 {
            let _ = writeln!(
                t,
                "# Other Compose files found: {}",
                compose[1..].join(", ")
            );
        }
        let _ = writeln!(t);
    }

    for s in &svcs {
        let _ = writeln!(t, "[services.{}]\n{}", s.name, s.body);
    }
    if svcs.is_empty() {
        let _ = writeln!(
            t,
            "# TODO: no .NET, Node, Go, Python or Spring Boot project found. A service looks like this:\n# [services.api]\n# build  = \"...\"\n# output = \"...\"\n# run    = \"... {{artifact}} ...\"\n# port   = 5080\n# health = {{ http = \"/health\", timeout = \"60s\" }}\n# watch  = [\"src/**\"]\n"
        );
    }
    let _ = writeln!(
        t,
        "# TODO: commands people and agents run (seed, reset); see the reference, section commands.\n"
    );
    let todos = t.matches("TODO").count();
    Ok(Draft {
        file,
        compose_files: compose,
        services: svcs.into_iter().map(|s| s.name).collect(),
        todos,
        text: t,
    })
}

pub fn run(repo: &Path, print: bool, json: bool) -> Result<()> {
    let file = repo.join("devstack.toml");
    if file.exists() && !print {
        bail!(
            "{} exists; devstack init writes only a first draft",
            file.display()
        );
    }
    let d = draft(repo)?;
    if !print {
        std::fs::write(&d.file, &d.text)?;
    }
    if json {
        println!("{}", serde_json::to_string(&d)?);
        return Ok(());
    }
    if print {
        print!("{}", d.text);
        return Ok(());
    }
    println!(
        "wrote {}: {} compose file(s), {} service(s) ({}), {} TODO(s)",
        d.file.display(),
        d.compose_files.len(),
        d.services.len(),
        d.services.join(", "),
        d.todos
    );
    match devstack_config::from_str(&d.text, &d.file) {
        Ok(_) => println!(
            "it loads; now work through the TODOs, then `devstack check` and `devstack doctor`"
        ),
        Err(e) => {
            println!("work through the TODOs; `devstack check` says what is still wrong:\n{e}")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_port_changes_only_that_number() {
        assert_eq!(
            replace_number(
                "port = 5000\nrun = \"x --urls http://localhost:5000 --id 50001 -v 1.5000\"",
                5000,
                5001
            ),
            "port = 5001\nrun = \"x --urls http://localhost:5001 --id 50001 -v 1.5000\""
        );
    }

    #[test]
    fn names_never_end_up_empty() {
        assert_eq!(kebab("My.Api"), "my-api");
        assert_eq!(kebab("日本"), "app");
        assert_eq!(kebab_raw("日本"), "");
    }

    #[test]
    fn a_monorepos_packages_are_searched_nugets_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::create_dir_all(r.join("packages/web")).unwrap();
        std::fs::write(r.join("packages/web/package.json"), "{}").unwrap();
        assert!(files(r).iter().any(|p| p.ends_with("package.json")));
        let nuget = r.join("legacy/packages");
        std::fs::create_dir_all(nuget.join("Newtonsoft.Json.13.0.1")).unwrap();
        std::fs::write(nuget.join("Newtonsoft.Json.13.0.1/x.csproj"), "").unwrap();
        assert!(is_nuget_packages(&nuget));
        assert!(!files(r).iter().any(|p| p.ends_with("x.csproj")));
    }

    #[test]
    fn a_dotnet_and_a_node_project_become_services() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::create_dir_all(r.join("src/My.Api/Properties")).unwrap();
        std::fs::write(
            r.join("src/My.Api/My.Api.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk.Web\"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>",
        )
        .unwrap();
        std::fs::write(
            r.join("src/My.Api/Properties/launchSettings.json"),
            "{\"profiles\":{\"x\":{\"applicationUrl\":\"https://localhost:7001;http://localhost:5001\"}}}",
        )
        .unwrap();
        std::fs::create_dir_all(r.join("src/My.Api.Tests")).unwrap();
        std::fs::write(
            r.join("src/My.Api.Tests/My.Api.Tests.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk.Web\"/>",
        )
        .unwrap();
        std::fs::create_dir_all(r.join("web/node_modules/x")).unwrap();
        std::fs::write(r.join("web/package.json"), "{\"name\":\"@acme/web\",\"scripts\":{\"dev\":\"vite\"},\"devDependencies\":{\"vite\":\"7\"}}").unwrap();
        std::fs::write(
            r.join("web/node_modules/x/package.json"),
            "{\"scripts\":{\"dev\":\"x\"}}",
        )
        .unwrap();

        let d = draft(r).unwrap();
        assert_eq!(d.services, vec!["my-api", "web"], "{}", d.text);
        assert!(
            d.text.contains("run    = \"dotnet {artifact}/My.Api.dll\""),
            "{}",
            d.text
        );
        assert!(
            d.text.contains("port   = 7001") || d.text.contains("port   = 5001"),
            "{}",
            d.text
        );
        assert!(
            d.text
                .contains("ASPNETCORE_URLS = \"http://127.0.0.1:{port}\"")
        );
        assert!(d.text.contains("port   = 5173"), "vite");
        assert!(d.todos > 0);
        devstack_config::from_str(&d.text, &r.join("devstack.toml")).expect("the draft loads");
    }

    #[test]
    fn go_python_and_spring_boot_become_services() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let w = |p: &str, t: &str| {
            let f = r.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, t).unwrap();
        };
        w("svc/go.mod", "module example.com/svc\n");
        w("svc/cmd/orders/main.go", "package main\n\nfunc main() {}\n");
        w("svc/cmd/orders/server.go", "package main\n");
        w("svc/internal/db/db.go", "package db\n");
        w(
            "api/pyproject.toml",
            "[project]\ndependencies = [\"fastapi\", \"uvicorn\"]\n",
        );
        w(
            "api/app/main.py",
            "from fastapi import FastAPI\napp = FastAPI()\n",
        );
        w("admin/manage.py", "import django\n");
        w(
            "billing/pom.xml",
            "<project><artifactId>billing-service</artifactId><dependency><artifactId>spring-boot-starter-web</artifactId></dependency><dependency><artifactId>spring-boot-starter-actuator</artifactId></dependency></project>",
        );
        w("billing/mvnw", "");

        let d = draft(r).unwrap();
        for n in ["orders", "api", "admin", "billing-service"] {
            assert!(
                d.services.iter().any(|s| s == n),
                "{n} in {:?}\n{}",
                d.services,
                d.text
            );
        }
        assert!(
            d.text
                .contains("cd svc && go build -o bin/orders/ ./cmd/orders"),
            "{}",
            d.text
        );
        assert!(
            d.text.contains("uvicorn app.main:app --reload"),
            "{}",
            d.text
        );
        assert!(d.text.contains("python manage.py runserver"), "{}", d.text);
        assert!(
            d.text.contains("./mvnw spring-boot:run")
                && d.text.contains("/actuator/health/readiness"),
            "{}",
            d.text
        );
        devstack_config::from_str(&d.text, &r.join("devstack.toml")).expect("the draft loads");
    }

    #[test]
    fn an_empty_repository_gets_a_loadable_skeleton() {
        let dir = tempfile::tempdir().unwrap();
        let d = draft(dir.path()).unwrap();
        assert!(d.services.is_empty());
        devstack_config::from_str(&d.text, &dir.path().join("devstack.toml")).expect("loads");
    }
}
