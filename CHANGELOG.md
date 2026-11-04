# Changelog

Notable changes per release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) as far as a 0.x release can.
Releases before 0.10.0 are summarised in [plan.md §7.4](docs/plan.md#74-phase-2-status)
and listed with their assets on the
[releases page](https://github.com/nuetzliches/devstack/releases).

## [Unreleased]

### Added

- **`devstack mcp` passes on the upstreams' instructions** (ADR-0069,
  addendum 2026-11-04). Its `initialize` answer carries, after devstack's
  own text, the `instructions` each MCP upstream gave when its session
  opened, under a heading with the prefix of its tools
  (`## \`tracker__\` (MCP server \`tracker\`)`) and quoted line by line.
  The hub asks the upstreams side by side for at most 3 seconds (new route
  `GET /hub/upstreams/instructions`), `devstack mcp` waits at most 5 in
  all; one that is slow, down or logged out is left out unless a session
  of it gave some before (kept in `upstream-instructions.json`, for the
  same URL or command only). At most 4000 characters per upstream and
  16 000 in all; control characters other than line ends and tabs are
  taken out. Instructions are fixed for a session: an upstream that
  changes them, or comes later, is seen in the next one.

### Fixed
