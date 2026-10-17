# ADR-0022: Outbound network calls of supervised services are visible and need an allow rule

- **Status:** Proposed (idea, not planned for a release yet)
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0007](0007-one-http-api-is-the-control-surface.md), [ADR-0009](0009-the-daemon-owns-every-process-tree.md), [ADR-0011](0011-platform-windows-native-first.md), [ADR-0014](0014-native-services-take-their-configuration-from-compose.md), [ADR-0019](0019-control-api-security.md), [ADR-0021](0021-a-hub-serves-the-dashboard.md)

## Context

A development stack is configured from the same files as the deployed one
([ADR-0014](0014-native-services-take-their-configuration-from-compose.md)).
Some of those values point at real external systems: a production API, a mail
gateway, a telephony provider, a payment or messaging service. One wrong or
missing override in a development env file and a local service sends real
mail, places real calls or writes into a real tenant. Nothing in today's
development setup shows that it happened; the developer finds out from the
recipient.

devstack already starts and owns every process tree
([ADR-0009](0009-the-daemon-owns-every-process-tree.md)) and sets each
service's environment. That makes it the natural place to see, and optionally
stop, outbound traffic per service. The idea: services started by the daemon
may not reach external hosts unless a rule allows it; blocked attempts are
listed in the dashboard and the API and can be allowed per service and host,
once or permanently.

Constraints:

- **No elevation in the default path.** devstack runs as the developer; an
  installer or a UAC prompt for every `devstack up` is not acceptable
  ([ADR-0011](0011-platform-windows-native-first.md): Windows-native first).
- **Local traffic must keep working:** loopback, published Compose ports, the
  dashboard hub, the OTLP sink, the IDE debugger.
- **Compose owns containers** ([ADR-0004](0004-infrastructure-is-delegated-to-compose.md),
  plan §9: devstack never networks containers itself).
- **devstack stays optional** ([ADR-0015](0015-devstack-is-optional.md)): a
  stack must still run without it, so nothing here may be required by the
  services' own configuration.

## Research: what is possible, per mechanism

### Cooperative: an egress proxy injected through the environment

The daemon runs an HTTP forward proxy on loopback and sets `HTTP_PROXY`,
`HTTPS_PROXY`, `NO_PROXY` (both upper- and lowercase; several runtimes read the
lowercase form first) in every service's environment. For HTTPS the client
sends `CONNECT host:port`; the proxy decides on host and port **without TLS
interception**, no certificate is installed anywhere. Plain HTTP is forwarded
with the full URL visible.

| Runtime | Honours the variables | Source |
|---|---|---|
| .NET `HttpClient` (default handler) | Yes, `HttpClient.DefaultProxy` reads `HTTP(S)_PROXY`, `ALL_PROXY`, `NO_PROXY` on Windows and Linux; env wins over the Windows user proxy setting. `NO_PROXY`: no `*` wildcards, leading dot for subdomains | [Microsoft Learn](https://learn.microsoft.com/en-us/dotnet/api/system.net.http.httpclient.defaultproxy) |
| Node.js `fetch`, `node:http(s)` | Only with `NODE_USE_ENV_PROXY=1` (or `--use-env-proxy`), Node 22.21+ / 24.5+; a custom `Agent` bypasses it. Older Node: undici's `EnvHttpProxyAgent` as global dispatcher | [Node.js docs](https://nodejs.org/learn/http/enterprise-network-configuration), [nodejs/node#58980](https://github.com/nodejs/node/pull/58980), [undici](https://undici.nodejs.org/#/docs/api/EnvHttpProxyAgent.md) |
| Python `requests`, `httpx` | Yes by default (`trust_env`) | [requests](https://requests.readthedocs.io/en/latest/user/advanced/#proxies), [httpx](https://www.python-httpx.org/environment_variables/) |
| Go `net/http` | Yes (`ProxyFromEnvironment` in the default transport); loopback is never proxied, regardless of `NO_PROXY` | [x/net/http/httpproxy](https://pkg.go.dev/golang.org/x/net/http/httpproxy), [golang/go#51416](https://github.com/golang/go/issues/51416) |
| Java | No. Needs `-Djava.net.useSystemProxies=true` or `-Dhttps.proxyHost/Port` (injectable via `JAVA_TOOL_OPTIONS`) | [Microsoft Learn (Java SDK)](https://learn.microsoft.com/azure/developer/java/sdk/proxying), [OpenJDK net-dev](https://mail.openjdk.org/pipermail/net-dev/2019-September/013145.html) |

What escapes: anything that is not an HTTP client honouring the variables.
SMTP, database and broker drivers, gRPC stacks with their own transport,
WebSocket libraries with their own agent, raw sockets, DNS (including
DNS-over-HTTPS from libraries that ignore the proxy), and any code that builds
its own handler with `UseProxy = false`. A cooperative proxy is a **seat belt
for HTTP**, not a sandbox.

Strength: weak (opt-out by accident or on purpose). Cost: low; no privileges,
identical on Windows and Linux, per-service attribution for free (one proxy
listener or one credential per service, see Decision).

### Enforced on Windows

- **Windows Filtering Platform (WFP).** Filters at the ALE connect layers can
  block or permit outbound connections. User-mode filter conditions identify a
  process by **application path** (`FWPM_CONDITION_ALE_APP_ID`, from
  `FwpmGetAppIdFromFileName0`), user SID or AppContainer package SID, not by
  process id or Job Object; a filter on an app id does not cover the process's
  children ([conditions](https://learn.microsoft.com/en-us/windows/win32/fwp/filtering-condition-identifiers-),
  [permitting and blocking applications](https://learn.microsoft.com/en-us/windows/win32/fwp/permitting-and-blocking-applications-and-users),
  [OSR thread](https://community.osr.com/t/how-to-get-original-process-id-or-process-path-in-windows-filtering-platform/52766)).
  Adding filters needs administrator rights in practice (the filter engine's
  default access). A dynamic session removes its filters when the session
  handle closes, which suits an owner process. Per-service scoping by path
  works for a service that runs its own executable from a per-build artifact
  directory ([ADR-0002](0002-processes-run-from-immutable-artifacts.md)), not
  for services that share `node.exe` or `dotnet.exe`. Scoping by process tree
  needs a **callout driver** (kernel code, signing): out of scope.
- **Windows Firewall rules per program path** (`New-NetFirewallRule -Program`,
  [docs](https://learn.microsoft.com/en-us/powershell/module/netsecurity/new-netfirewallrule)):
  same path limitation, needs elevation, and rules persist after a crash unless
  cleaned up. No per-user firewall store exists.
- **AppContainer / LPAC.** A process started in an AppContainer has no network
  unless granted `internetClient` / `privateNetworkClientServer`; starting one
  does not need elevation
  ([implementing an AppContainer](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer),
  [LPAC overview](https://hackmag.com/stuff/windows-lpac)). It is the only
  unprivileged enforcement on Windows, and it is all or nothing: loopback to
  other processes is blocked (exemptions need admin), and file system, registry
  and named-object access are restricted too, which breaks ordinary .NET and
  Node services. Useful only as an identity: a per-service package SID that WFP
  filters can match, combined with the elevated helper below.

### Enforced on Linux

- **cgroup v2 BPF (`cgroup/connect4`, `connect6`, `sendmsg4/6`).** A
  `BPF_PROG_TYPE_CGROUP_SOCK_ADDR` program attached to the service's cgroup
  sees every `connect()` (and UDP `sendmsg`) of every process in it and can
  deny it or **rewrite the destination**, e.g. to the devstack proxy
  ([ebpf docs](https://docs.ebpf.io/linux/program-type/BPF_PROG_TYPE_CGROUP_SOCK_ADDR/)).
  It matches devstack's per-service cgroup from ADR-0009 exactly. Needs
  `CAP_BPF` + `CAP_NET_ADMIN` (or root) and a delegated cgroup.
- **nftables `socket cgroupv2`** (kernel 5.13+): matches the owning cgroup of a
  socket, recursively. The path is resolved to an id when the rule loads, so the
  cgroup must exist first ([write-up](https://blog.fraggod.net/2021/08/31/easy-control-over-applications-network-access-using-nftables-and-systemd-cgroup-v2-tree.html)).
  Needs `CAP_NET_ADMIN`; matches IPs, not host names.
- **Landlock network rules** (kernel 6.7+, ABI 4): an unprivileged process can
  restrict itself and its descendants to TCP `connect` on allowed **ports**
  ([kernel docs](https://www.kernel.org/doc/html/latest/userspace-api/landlock.html)).
  The shim could apply it before `exec`: allow only the proxy port and the
  published infrastructure ports. No host names, no IPs, TCP only; a bypass via
  TCP Fast Open was reported in 2026
  ([LSM list](https://kernsec.org/pipermail/linux-security-module-archive/2026-June/058089.html)).
- **Network namespaces.** `unshare` into a user + network namespace (no root)
  and give it controlled connectivity with `pasta`/`slirp4netns`
  ([pasta(1)](https://man.archlinux.org/man/pasta.1.en)). Strong, but localhost
  in the namespace is no longer the host's: every published Compose port and the
  hub must be forwarded explicitly, and attaching a debugger or profiler gets
  harder.
- **seccomp** can refuse `socket`/`connect` but sees no addresses behind
  pointers; too coarse alone.

### Containers (infrastructure)

A Compose network with `internal: true` has no outbound connectivity, but a
service attached to any other network still has
([Compose networking](https://docs.docker.com/compose/how-tos/networking/)),
and published ports are what native services use to reach infrastructure.
Changing container networks contradicts ADR-0004 and plan §9; it stays the
project's own Compose decision.

### Prior art

- [Little Snitch](https://www.obdev.at/products/littlesnitch/) (macOS),
  [OpenSnitch](https://github.com/evilsocket/opensnitch) (Linux, eBPF): a prompt
  per process and destination, the decision remembered. The interaction model
  this ADR copies.
- [Harden-Runner](https://github.com/step-security/harden-runner) for GitHub
  Actions: **audit** mode first (log every destination, recommend a policy),
  then **block** mode with an allow list
  ([blocked endpoints](https://docs.stepsecurity.io/github-actions/harden-runner)).
  Its DNS-over-HTTPS and DNS-over-TCP bypasses show where egress control leaks.
- [Claude Code sandboxing](https://code.claude.com/docs/en/sandboxing): a proxy
  outside the sandbox with a domain allow list and a prompt for new domains,
  enforced by bubblewrap (Linux) and Seatbelt (macOS); native Windows is not
  supported. Cooperative proxy plus OS enforcement, the same split as below.
- The [Claude Code devcontainer firewall](https://github.com/anthropics/claude-code/blob/main/.devcontainer/init-firewall.sh):
  default-deny iptables with an ipset of resolved allowed domains, inside a
  privileged container. Shows that IP-based rules need re-resolution.

## Decision (proposed)

devstack gets an **egress control** feature, built in stages. Each stage ships
only if the previous one proved useful.

**Stage 1: cooperative egress proxy, visible and controllable. No privileges,
Windows and Linux alike.**

1. The daemon runs one forward proxy on loopback (HTTP and `CONNECT`, no TLS
   interception). Every service it starts gets `HTTP_PROXY`, `HTTPS_PROXY`,
   `NO_PROXY` in both cases, `NODE_USE_ENV_PROXY=1`, and for Java services a
   `JAVA_TOOL_OPTIONS` fragment. The service is identified by a per-service
   credential in the proxy URL (`http://svc-api:<token>@127.0.0.1:<port>`) or a
   per-service listener port; the decision between the two is open.
2. `NO_PROXY` always contains `localhost,127.0.0.1,::1` and the hosts devstack
   itself rewrote ([ADR-0014](0014-native-services-take-their-configuration-from-compose.md));
   everything on loopback is local by definition.
3. Mode per stack: `audit` (default at introduction: allow, record) or `block`
   (deny unless a rule allows; the client gets `403` with a body naming
   devstack and the rule to add). An optional short **hold** lets the dashboard
   answer a pending `CONNECT` before it is denied.
4. Rules: scope `service` or `stack`; host pattern (exact or `*.example.org`);
   optional port; duration `once` (this connection), `session` (until the daemon
   stops) or `permanent`. Permanent rules live in the stack file
   (`[egress] allow = [...]`, reviewable, shared with the team) or in the user's
   local state (personal, not committed).
5. An existing upstream proxy from the developer's environment is chained, not
   replaced.
6. The feature is labelled **"HTTP egress visibility (cooperative)"** everywhere
   it appears. The UI never says "sandboxed" in stage 1.

**Stage 2: optional enforcement, where privileges exist.**

- **Linux:** with a delegated cgroup and `CAP_BPF`/`CAP_NET_ADMIN` (granted to
  a small helper, never to the daemon as a whole), a `cgroup/connect4/6`
  program on each service's cgroup redirects non-loopback TCP connects to the
  proxy and refuses everything else not allowed by rule. Without privileges, on
  kernel 6.7+, the shim applies **Landlock** (connect only to the proxy and
  declared infrastructure ports) as an unprivileged middle step.
- **Windows:** an **optional elevated helper** (installed once, a service
  holding a dynamic WFP session) adds block filters for each service's
  executable path, or for a per-service AppContainer package SID if that proves
  workable. Without the helper, Windows stays at stage 1, and the dashboard says
  so.
- Containers are not covered by either stage.

### Data and surfaces

| Record | Fields |
|---|---|
| Egress attempt | service, slot/lane, host, port, scheme (`CONNECT`/HTTP), decision (`allowed`/`blocked`/`held`), matching rule, first and last seen, count. Aggregated per (service, host, port), not one row per request |
| Allow rule | id, scope (service/stack), host pattern, port, duration (once/session/permanent), origin (stack file/user/dashboard), created by, created at, hits |
| Escape signal (stage 2) | service, destination IP, port, protocol, what blocked it (BPF/Landlock/WFP) |

- **API** ([ADR-0007](0007-one-http-api-is-the-control-surface.md)):
  `GET /v1/egress/attempts`, `GET/POST/DELETE /v1/egress/rules`,
  `PUT /v1/egress/mode`; SSE events `egress.blocked`, `egress.allowed`,
  `egress.rule_changed` on `/v1/events`.
- **Dashboard** (through the hub, [ADR-0021](0021-a-hub-serves-the-dashboard.md)):
  an "Outbound" view per stack and a badge on each service with blocked
  attempts; one click turns an attempt into a rule; a tray notification for the
  first block of a host.
- **MCP:** `egress_attempts` and `egress_rules` read tools, so an agent sees
  why a call failed. Creating a permanent rule through MCP needs confirmation in
  the dashboard; an agent must not unblock production systems by itself.
- **CLI:** `devstack egress` (list, allow, deny), `devstack env <service>` shows
  the injected proxy variables.

## Alternatives considered

- **Enforcement first (stage 2 only).** Needs elevation on Windows and
  capabilities on Linux before anyone knows whether the feature is wanted;
  contradicts the no-admin default.
- **AppContainer for every service.** The only unprivileged hard block on
  Windows, but it cuts loopback and file access; services would not start.
- **Network namespace per service on Linux.** Strong and unprivileged with
  pasta, but turns every localhost assumption (Compose ports, hub, debugger)
  into explicit forwarding; out of proportion for stage 1.
- **TLS-intercepting proxy.** Would show paths and bodies, but needs a trusted
  root certificate per runtime (each has its own store) and sees credentials.
  Host-level decisions do not need it.
- **DNS-based blocking.** Cheap, but cached addresses, literal IPs and DoH pass;
  Harden-Runner's bypasses show the limits.
- **Leave it to the project** (dummy hosts in development env files). Keeps
  working where someone remembered; fails silently where no one did, which is
  the problem.

## Consequences

- **False sense of security** is the main risk of stage 1. A blocked HTTP call
  says nothing about the SMTP connection next to it. Mitigation: the label, a
  documented list of what escapes, and in `block` mode a hint per service that
  has non-HTTP dependencies in its Compose configuration.
- **Breaking local traffic.** A host missing from `NO_PROXY` (a LAN service, a
  container reached by a non-loopback address, an IPv6 literal) is sent to the
  proxy and blocked. `NO_PROXY` syntax differs between runtimes (.NET: no `*`,
  leading dot only; Go: CIDR allowed, loopback always direct); devstack writes
  the lowest common form and shows the result in `devstack env`.
- **Configuration conflicts:** a Compose `environment` that already sets proxy
  variables; the stack file decides (ADR-0014 order), and `devstack check`
  warns.
- **Performance:** one extra loopback hop per connection; `CONNECT` tunnels are
  byte copies after the handshake. Expected negligible for development; to be
  measured with a service doing many short connections.
- **The daemon becomes a network component.** The proxy listener is a new local
  attack surface: loopback only, per-service credentials, no open relay, same
  stance as [ADR-0019](0019-control-api-security.md).
- **Stage 2 adds a privileged component** to an otherwise unprivileged tool,
  with installation, update and uninstall to own. That is why it is optional
  and last.
- The feature must stay switchable off per stack and per service; a stack must
  run identically without devstack ([ADR-0015](0015-devstack-is-optional.md)),
  so nothing in the services may depend on the proxy.

## Open questions

1. Service identity at the proxy: credential in the proxy URL (one listener,
   but some clients drop or log proxy credentials) or one listener port per
   service (unambiguous, more ports)?
2. Default mode for new stacks once the feature exists: `audit` or `block`?
3. Where permanent rules live by default: stack file (shared) or user state
   (personal)? Both, with the stack file as the reviewable baseline?
4. Should non-HTTP dependencies (SMTP, external databases) be declared in the
   stack file so the dashboard can show them as "not covered" explicitly?
5. Stage 2 on Windows: does an AppContainer with `internetClient` and loopback
   to the proxy run real .NET/Node services well enough to give WFP a
   per-service SID? Needs a spike before the helper is designed.
6. Do builds (package restore) go through the proxy too? Probably audit only,
   since registries are expected traffic.
7. Lanes and candidates: do rules apply per service across slots (likely), and
   are attempts attributed to the slot that made them?

## Enforced by

Nothing yet; this ADR is an idea. When built it will live in:

- a `devstack-egress` crate (proxy, rule matching, attempt aggregation);
- the supervisor's environment assembly (`crates/devstack-daemon`, next to the
  ADR-0014 merge) for the injected variables;
- `crates/devstack-proc` (Linux shim) for Landlock and the cgroup BPF hookup in
  stage 2;
- the control API, the SSE event types, the dashboard's Outbound view and the
  MCP tool list;
- an integration test that starts a .NET and a Node service with
  `NODE_USE_ENV_PROXY`, calls an external host and asserts the recorded,
  blocked attempt.

*Noted 2026-10-18, against a name clash.* Nothing of this ADR is built.
`crates/devstack-node/src/egress.rs` is not it: that module sets iptables
rules per lane on a node of the pool, for the node's network mode
(`internet`, `none`; [ADR-0033](0033-a-node-decides-what-runs-on-it.md)
points 5 and 9). It confines a whole lane on someone else's machine by
address range, sees no attempts and has no allow rules per service; this ADR
is about native services on the developer's own machine. Should this ADR be
built, a `devstack-egress` crate beside the node's `egress` module would read
as one thing; one of them gets another name then.

## What this ADR does not say

- It does not make devstack a security boundary against malicious code. Stage
  1 catches mistakes in cooperative code; stage 2 narrows the escapes, it does
  not close them all (UDP, DNS, kernel bypasses).
- It does not change container networking.
- It does not inspect request contents.
