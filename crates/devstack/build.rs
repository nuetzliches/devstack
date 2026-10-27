//! The `devstack` binary's main thread gets 8 MiB of stack on Windows, as
//! it has on Linux: Windows gives 1 MiB, which clap's command tree for every
//! subcommand overflows in an unoptimised build.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows-msvc") {
        println!("cargo:rustc-link-arg-bin=devstack=/STACK:8388608");
    } else if target.contains("windows-gnu") {
        println!("cargo:rustc-link-arg-bin=devstack=-Wl,--stack,8388608");
    }
}
