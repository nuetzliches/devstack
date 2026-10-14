//! Embeds the dashboard (`ui/dist`, built with `npm run build` in `ui/`). Without
//! a build, a placeholder page explains how to get one, so the Rust side builds
//! without Node.

use std::path::Path;

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            copy(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
}

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let dist = Path::new(&manifest).join("../../ui/dist");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("ui");
    let _ = std::fs::remove_dir_all(&out);
    println!("cargo:rerun-if-changed=../../ui/dist");
    if dist.join("index.html").exists() {
        copy(&dist, &out);
    } else {
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            out.join("index.html"),
            "<!doctype html><meta charset=utf-8><title>devstack</title>             <body style=\"font-family:sans-serif;max-width:40em;margin:4em auto\">             <h1>devstack hub</h1><p>This build has no dashboard. Build it with              <code>npm ci &amp;&amp; npm run build</code> in <code>ui/</code>, then rebuild devstack.</p>",
        )
        .unwrap();
    }
}
