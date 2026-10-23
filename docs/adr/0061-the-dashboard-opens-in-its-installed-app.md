# ADR-0061: The dashboard opens in its installed app, when there is one

- **Status:** Proposed
- **Date:** 2026-10-23
- **Deciders:** project owner
- **Related:** [ADR-0018](0018-dashboard-is-a-browser-page.md), [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the hub's origin, the PWA, pairing), [ADR-0045](0045-the-hub-notifies-the-desktop.md) (a click on a notification), [ADR-0060](0060-the-tray-is-a-client-of-the-hub.md) (the tray's *Open Dashboard*), [ADR-0011](0011-platform-windows-native-first.md), [ADR-0054](0054-macos-is-a-developer-platform-in-preview.md)

## Context

The hub serves the dashboard as a progressive web app on a fixed origin,
`http://127.0.0.1:4250` (ADR-0021), and a Chromium-family browser (Chrome,
Edge, Brave, Chromium) installs it as an app with a window of its own. Every
way devstack opens the dashboard, `devstack ui`, the tray's *Open Dashboard*
and its per-stack entries, a click on a notification and a
`devstack://open` link, hands the pairing link to the default browser
(`xdg-open`, the shell on Windows, `open` on macOS), or to `--browser`,
`DEVSTACK_BROWSER` or `BROWSER`.

The browser opens that link in a tab. Chromium captures links into an
installed app only on some platforms and only behind a setting; on Linux it
does not at all. So a user who installed the dashboard as an app, to have it
beside their editor as a window of its own, gets a second copy in a tab each
time they click the tray icon.

Two facts make a precise answer cheap:

- **An installed web app's id follows from its manifest id.** Chromium names
  an app by the hash of its manifest id (the manifest's `id`, or its
  `start_url`, resolved against the origin): the SHA-256 of the SHA-256 of the
  URL, its first 16 bytes written with the letters `a` to `p`. The
  dashboard's manifest id is `http://127.0.0.1:<hub port>/`, so the app id is
  known without reading the browser's data, and it is the same in every
  Chromium browser and profile. Checked: an app installed by Brave from the
  hub has the id this computes.
- **Every Chromium-family browser starts an installed app by that id:**
  `--profile-directory=<profile> --app-id=<id>`, and
  `--app-launch-url-for-shortcuts-menu-item=<url>` opens it on a URL in the
  app's scope, as a shortcut of the app's jump list does. A browser already
  running hands the command line to its running instance.

## Decision

1. **devstack opens the dashboard in its installed app when it finds one,
   else in the browser as before.** This holds for every way it opens the
   dashboard: `devstack ui`, the tray, a click on a notification (the hub's
   own on Linux, a `devstack://open` link elsewhere). `--browser`,
   `DEVSTACK_BROWSER` and `BROWSER` still win: naming a browser means that
   browser, in a tab.
2. **What counts as the dashboard's app is decided by its id**, computed from
   the manifest id `http://127.0.0.1:<hub port>/`. An app installed from
   `localhost` has another id and origin, and another cookie, and is not
   taken. The manifest names its `id` (`/`), so a later change of
   `start_url` does not change the app's id.
3. **Where devstack looks, per platform:**
   - **Linux:** the launchers Chromium writes on install, the `.desktop`
     files in `$XDG_DATA_HOME/applications` (`~/.local/share/applications`)
     and in the desktop folder (`XDG_DESKTOP_DIR` from `user-dirs.dirs`,
     else `~/Desktop`). A launcher is the app's when its `Exec` line has
     `--app-id=<id>`; it gives the browser's program and the profile
     (`--profile-directory`). A program under `/snap/<name>/<revision>/` is
     started as `/snap/bin/<name>`, since the revision in the launcher goes
     stale with the snap's updates; a program that no longer exists is
     looked up on `PATH` by the browser the launcher's file name names
     (`brave-…`, `chrome-…`, `chromium-…`, `msedge-…`).
   - **Windows:** the browsers' profile folders under `%LOCALAPPDATA%`
     (Chrome, Edge, Brave, Chromium, `User Data\<profile>`): the app is
     installed in a profile that has `Web Applications\Manifest
     Resources\<id>`. The browser is its `…\Application\<browser>.exe` under
     `%ProgramFiles%`, `%ProgramFiles(x86)%` or `%LOCALAPPDATA%`. The Start
     menu shortcut is not read: it is a `.lnk`, and the profile folder says
     the same.
   - **macOS: not yet.** No maintainer has a Mac to check Chromium's app
     shims (`~/Applications/<Browser> Apps.localized/`) against; the
     dashboard opens in the browser as before.
   When several launchers or profiles have the app, the first found is
   taken (Linux: the applications folder before the desktop folder;
   Windows: Chrome, Edge, Brave, Chromium, and `Default` before the other
   profiles).
4. **The app is started on the pairing link:**
   `<browser> --profile-directory=<profile> --app-id=<id>
   --app-launch-url-for-shortcuts-menu-item=<pairing link>`. The link is in
   the app's scope; the app shares the profile's cookies, so pairing it
   pairs that profile, as before. The program is started directly, with no
   shell in between.
5. **The open window is reused:** the manifest says
   `launch_handler: { client_mode: "navigate-existing" }`, so a launch
   navigates the app's open window to the link instead of opening a second
   one. A browser that does not know `launch_handler` opens a window.
6. **A launch that fails falls back to the browser**, as if no app were
   installed; nothing waits for the app's window.

## Alternatives considered

- **Leave it to the browser's link capturing.** Not on Linux, and elsewhere
  behind a per-app setting the user has to find.
- **Read the browser's web app database** (`Web Applications`, a LevelDB of
  protocol buffers) to match the app by its start URL. A second format to
  parse per browser for an answer the id already gives.
- **Match the launcher by its name (`Name=devstack`).** The user can rename
  an app, and another app may be called the same.
- **A setting naming the app's command** (`DEVSTACK_APP`). Possible later
  for a browser this does not find; the user would have to find the app id
  first.
- **Open the app without the pairing link** (`--app-id` alone, the start
  URL). The app is usually paired already, but a stack's route
  (`/#/s/<id>?lane=`) would be lost, and an unpaired profile would land on
  the pairing notice.

## Consequences

- A user with the dashboard installed as an app gets the app from the tray,
  from `devstack ui` and from a notification, and one window of it.
- Firefox has no installable web apps; nothing changes for it.
- Chromium's flags and folders are not a published interface. If a browser
  changes them, devstack falls back to the browser tab, which is today's
  behaviour.
- On Windows the reused window is a Chromium decision (`launch_handler`)
  devstack cannot test in CI.

## Enforced by

- `crates/devstack-desktop/src/webapp.rs`: the app id, the launcher's
  parsing and matching (with tests); `linux.rs`, `windows.rs`, `macos.rs`:
  `dashboard_app`.
- `crates/devstack/src/hubcmd.rs`: `open_dashboard` and `devstack ui`;
  `crates/devstack-hub/src/lib.rs`: a notification's click.
- `ui/vite.config.ts`: the manifest's `id` and `launch_handler`.
