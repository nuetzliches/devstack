//! Spike (a): process ownership (plan §7.1, ADR-0009, ADR-0016).
//!
//! `procspike run` starts services the way the daemon will (one Job Object per
//! service on Windows), records which processes belong to them, holds, then
//! stops them gracefully and escalates to a kill. The test harness
//! (`run-tests.mjs`) drives it through the scenarios a1-a8 and checks the
//! outcome from the outside.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(windows)]
mod win;

#[derive(Parser)]
#[command(about = "Spike (a): process ownership")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum StopMethod {
    /// CTRL_BREAK to the service's process group (shares our console)
    Break,
    /// Ctrl+C written into a pseudo console per service
    Conpty,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start services, record their processes, hold, stop.
    Run {
        #[arg(long, value_enum, default_value = "conpty")]
        method: StopMethod,
        /// Seconds to wait for a graceful stop before killing the job.
        #[arg(long, default_value_t = 20)]
        stop_timeout: u64,
        /// Seconds to run before stopping; 0 = run until killed.
        #[arg(long, default_value_t = 10)]
        hold: u64,
        /// Directory for the lock, logs, pids.json and report.json.
        #[arg(long)]
        state: PathBuf,
        /// Negative control: make the job handle inheritable (ADR-0009 condition 1).
        #[arg(long)]
        inheritable_job: bool,
        /// Negative control: allow breakaway from the job (ADR-0009 condition 2).
        #[arg(long)]
        allow_breakaway: bool,
        /// Allocate a hidden console when started without one (detached daemon).
        #[arg(long)]
        alloc_console: bool,
        /// Services as name=command line.
        #[arg(required = true)]
        services: Vec<String>,
    },
    /// Fixture: try to start a child outside the current job.
    Breakaway {
        /// File to write the outcome to.
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Serialize)]
struct StopReport {
    service: String,
    graceful_signal_ok: bool,
    signal_error: Option<String>,
    stopped_gracefully: bool,
    stop_ms: u128,
    killed: bool,
    /// Unix ms; lets the harness check that a signal reached only its service.
    requested_at_ms: u128,
}

fn unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            method,
            stop_timeout,
            hold,
            state,
            inheritable_job,
            allow_breakaway,
            alloc_console,
            services,
        } => {
            #[cfg(windows)]
            if alloc_console {
                win::ensure_hidden_console()?;
            }
            #[cfg(not(windows))]
            let _ = alloc_console;
            run(
                method,
                stop_timeout,
                hold,
                state,
                inheritable_job,
                allow_breakaway,
                services,
            )
        }
        Cmd::Breakaway { out } => breakaway(out),
    }
}

#[cfg(windows)]
fn run(
    method: StopMethod,
    stop_timeout: u64,
    hold: u64,
    state: PathBuf,
    inheritable_job: bool,
    allow_breakaway: bool,
    services: Vec<String>,
) -> Result<()> {
    fs::create_dir_all(&state)?;

    // ADR-0016: single instance through an OS lock on an open handle. The OS
    // releases it on any exit, so a stale lock cannot exist.
    let lock = File::create(state.join("lock"))?;
    if lock.try_lock().is_err() {
        eprintln!("locked: another instance owns {}", state.display());
        std::process::exit(3);
    }

    let mut started = Vec::new();
    for spec in &services {
        let (name, cmdline) = spec
            .split_once('=')
            .context("service must be name=command")?;
        let log = File::create(state.join(format!("{name}.log")))?;
        let job = win::Job::new(inheritable_job, allow_breakaway)?;
        let svc = match method {
            StopMethod::Break => win::spawn_break(name, cmdline, log, job)?,
            StopMethod::Conpty => win::spawn_conpty(name, cmdline, log, job)?,
        };
        started.push(svc);
    }

    // Let the trees grow (wrappers start their children), then record them.
    std::thread::sleep(Duration::from_secs(4));
    let pids: BTreeMap<String, Vec<u32>> = started
        .iter()
        .map(|s| (s.name.clone(), s.job.pids()))
        .collect();
    fs::write(state.join("pids.json"), serde_json::to_vec_pretty(&pids)?)?;
    fs::write(state.join("supervisor.pid"), std::process::id().to_string())?;

    if hold == 0 {
        // Run until killed; the test kills us hard.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    std::thread::sleep(Duration::from_secs(hold));

    // Stop one service at a time so the harness can check that a signal for
    // one service does not reach the others.
    let mut reports = Vec::new();
    for svc in &mut started {
        let t0 = Instant::now();
        let requested_at_ms = unix_ms();
        let signal = svc.request_stop();
        let deadline = t0 + Duration::from_secs(stop_timeout);
        while svc.job.active_processes() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let gracefully = svc.job.active_processes() == 0;
        if !gracefully {
            svc.job.terminate();
        }
        reports.push(StopReport {
            service: svc.name.clone(),
            graceful_signal_ok: signal.is_ok(),
            signal_error: signal.err().map(|e| e.to_string()),
            stopped_gracefully: gracefully,
            stop_ms: t0.elapsed().as_millis(),
            killed: !gracefully,
            requested_at_ms,
        });
    }
    fs::write(
        state.join("report.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "method": method,
            "stops": reports,
        }))?,
    )?;
    Ok(())
}

#[cfg(not(windows))]
fn run(_: StopMethod, _: u64, _: u64, _: PathBuf, _: bool, _: bool, _: Vec<String>) -> Result<()> {
    anyhow::bail!("the Linux half of spike (a) is not written yet")
}

/// Try to start a long-running child outside our job and report what happened.
fn breakaway(out: PathBuf) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let r = std::process::Command::new("ping")
            .args(["-n", "120", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
            .spawn();
        let text = match r {
            Ok(child) => format!("escaped {}", child.id()),
            Err(e) => format!("refused {e}"),
        };
        fs::write(&out, &text)?;
        println!("{text}");
        // Stay alive so the service has something to stop.
        std::thread::sleep(Duration::from_secs(600));
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = out;
        anyhow::bail!("Windows only")
    }
}
