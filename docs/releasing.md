# Releasing devstack

A release is a pull request `chore: X.Y.Z` merged to `main`, then an
annotated tag `vX.Y.Z` on its merge commit. The tag starts
[`release.yml`](../.github/workflows/release.yml): it builds every
platform, publishes the GitHub release and hands the builds to winget and
Homebrew. Nothing is released by the merge alone.

Versions follow [Semantic Versioning](https://semver.org/) as far as a 0.x
release can: a new key or feature raises the minor version, fixes alone the
patch version.

## 1. Before the release pull request

- [ ] **What goes in.** `CHANGELOG.md` under `[Unreleased]` lists every
      change on `main` since the last tag (`git log vA.B.C..origin/main`).
      Open pull requests that belong in this release are merged first;
      the rest wait for the next one.
- [ ] **Checked on every platform.** A pull request that says it was not
      checked on Windows (or on a Mac) is checked there before the release:
      Windows is first-class ([ADR-0011](adr/0011-platform-windows-native-first.md)).
      macOS is a preview ([ADR-0054](adr/0054-macos-is-a-developer-platform-in-preview.md));
      what is not checked on a Mac goes into the release pull request's
      description, and [macos.md](macos.md) has the checklist for testers.
- [ ] **Decisions.** Every ADR the release builds on is `Accepted`, and
      [the ADR index](adr/README.md) says the same (also where another
      ADR's line names it).
- [ ] **CI on `main` is green.** A test that fails now and then is fixed in
      a pull request of its own, not in the release one; to get the release
      through, its failed job is run again (`gh run rerun <run> --failed`).

## 2. The release pull request

On a branch `chore/X.Y.Z` from `main`:

- [ ] **Version.** `version` under `[workspace.package]` in `Cargo.toml`,
      then `cargo update -w` (the workspace's own crates only, no other
      dependency in `Cargo.lock` moves); in `ui/`,
      `npm version X.Y.Z --no-git-tag-version` (`package.json` and
      `package-lock.json`). The release workflow refuses a tag that does
      not match both.
- [ ] **Changelog.** Below `## [Unreleased]`, a heading
      `## [X.Y.Z] - YYYY-MM-DD` with a summary paragraph (the release in a
      few sentences, then "Fixed: ..."); `[Unreleased]` stays, empty. At
      the end of the file, `[Unreleased]` compares from `vX.Y.Z`, and a
      line `[X.Y.Z]` compares the previous tag with `vX.Y.Z`.
- [ ] **Plan.** In [plan.md](plan.md), the status line names the release
      and its date ("status updated"), and §7.4 gets a paragraph
      `**X.Y.Z (date): ...**` like the ones before it.
- [ ] **Stack file versions.** Where the changelog says a key needs
      `requires = "X.Y.Z"`, the constant that `devstack check` uses for it
      (`*_SINCE` in `crates/devstack-config/src/lib.rs`) says the same.
- [ ] **The repository's own stack file and skills.** With the version
      raised, the binary built here is already `X.Y.Z`, so it writes what
      the release will:
      `cargo run -q -p devstack -- skills install` refreshes
      `.claude/skills/devstack*`, and `devstack.toml` gets
      `reviewed = "X.Y.Z"` in `[stack]`. Raise `requires` only when the
      file uses something new. `cargo run -q -p devstack -- check` is
      clean. Otherwise `main` lags one release behind, and the dashboard
      offers the upgrade right after the update.
- [ ] **Commit and pull request.** Title `chore: X.Y.Z`; the commit body
      is the changelog's summary paragraph. The description names what was
      checked on which platform and what was not.
- [ ] **CI green, then merge** (squash).

## 3. The tag

On the merge commit, which is the tip of `main`:

```bash
git fetch origin
git tag -a vX.Y.Z -m "devstack X.Y.Z" <merge-commit>
git push origin vX.Y.Z
```

The release workflow checks that the tag matches `Cargo.toml` and
`ui/package.json` and that the tagged commit is on `main`, then:

- builds Windows (x86_64), Linux (x86_64, arm64; static) and macOS (Apple
  Silicon, Intel);
- publishes the GitHub release: the five archives, `install.sh`,
  `install.ps1` and `SHA256SUMS`, each with a build provenance
  attestation, and notes generated from the merged pull requests;
- submits the Windows build to winget (no second pull request while the
  package's first one is open; afterwards
  `gh workflow run winget.yml -f tag=vX.Y.Z` submits by hand);
- tests the Homebrew formula on macOS and Linux and pushes it to
  `nuetzliches/homebrew-tap`.

A tag cannot be taken back without confusing everyone who fetched it: if
the workflow fails before publishing, fix it on `main` and tag the next
patch version.

## 4. After the release

- [ ] **The release is there**: not a draft, every asset listed, `devstack
      update` on a machine with the previous release finds it.
- [ ] **Issues.** An issue a pull request of this release fixed but only
      referenced ("Refs #N") is closed now, with "Fixed in
      [X.Y.Z](https://github.com/nuetzliches/devstack/releases/tag/vX.Y.Z)
      by #PR" and a line on what changed.
- [ ] **This machine.** `devstack update`, then `devstack down` and
      `devstack up` here: the stack runs the new daemon, and the dashboard
      shows neither the upgrade banner nor the skills banner.
