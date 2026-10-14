# ADR-0005: Rust for daemon and CLI

- **Status:** Accepted
- **Date:** 2026-10-14
- **Deciders:** project owner

## Context

The daemon supervises process trees on Windows and Linux, watches files, runs an
HTTP server with an event stream, and must keep running while the stacks it
supervises are rebuilt. A daemon written in the same runtime as the supervised
services (.NET) would share their failure modes, including locking its own
binaries when it is rebuilt. The sibling project Croniq is Rust with a Vue
dashboard; its server, auth and SSE patterns are reusable knowledge.

## Decision

Daemon, CLI and MCP server are written in Rust (stable, edition 2024) as one
Cargo workspace producing a single `devstack` binary with subcommands
(`devstack daemon`, `devstack mcp`, `devstack status`, …). Platform-specific
process control sits behind one internal trait with a Windows and a Unix
implementation.

## Alternatives considered

- **Go.** Equally suitable (single binary, good process handling, fsnotify).
  Rejected for consistency with Croniq and shared tooling.
- **.NET.** Familiar to the downstream team; rejected because the orchestrator
  must not share the runtime, build system and lock behaviour of what it
  orchestrates.

## Consequences

- Windows APIs (Job Objects, console control events) via `windows-sys`/`windows`.
- File watching via the `notify` crate (ReadDirectoryChangesW / inotify).

## Enforced by

- `Cargo.toml` workspace (to be created in phase 1).
