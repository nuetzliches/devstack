# Spike (b): build cancellation

Answers plan §7.2 for [ADR-0003](../../docs/adr/0003-changes-flow-through-candidates.md)
and [ADR-0009](../../docs/adr/0009-the-daemon-owns-every-process-tree.md): can a
superseded build be killed at any moment without breaking or slowing the next
one, and where do build servers belong?

## How

`run.mjs` keeps two clones of the reference stack at one commit. Each iteration
adds a new public type to a project almost everything depends on (the reference
assembly changes, every dependant recompiles: about 34 s), then

- **A** builds and is stopped at a random moment, then builds again to the end;
- **B** builds the same change without interruption.

Both map their source root to `/src` (`PathMap`), so deterministic compilation
makes A's and B's assemblies byte-identical unless something went wrong. Builds
run in a Job Object through `procspike exec` (`../process-ownership`), exactly
as the daemon will run them.

```bash
node run.mjs --scenario scenarios/<name>.local.json   # see scenarios/example.json
```

## Results (Windows 11, reference API host, 2026-10-14)

| # | Test | Result |
|---|---|---|
| b1 | 14 builds stopped at random moments between 0.45 s and 25 s (restore, compilation, copy) | **14/14 following builds succeeded, all assemblies byte-identical** with the uninterrupted build |
| b2 | Hard stop: time until no process of the build is left / until the output can be renamed | 2–10 ms / at most 33 ms |
| b3 | Following build compared with the uninterrupted one | at most 6 % slower, often 30–60 % faster (it reuses the work done before the stop) |
| b4 | Graceful stop (`CTRL_BREAK`) during compilation | 6–14 s until MSBuild has wound down; no benefit for the following build |
| b5 | Build servers (MSBuild node reuse, shared compiler) **in the build's job**, killed after every build (32 processes each time) | 31–35 s per build |
| b5 | Build servers **outside** the build's job, staying warm | 29 s, then 12–16 s |
| b5 | Build servers **off** (`-nodeReuse:false -p:UseSharedCompilation=false`) | 42–43 s |
| b6 | SPA production build stopped during bundling, then rebuilt | succeeds; only `ngsw.json` differs, which also differs between two uninterrupted builds (it carries a build timestamp) |

Also found on the way: a build's root process exits while the build servers it
started stay in its job, so **"the job is empty" is not "the build is done"**;
`procspike exec` treats the root's exit as the end and terminates what is left.

## Findings for the daemon

1. **Cancel builds hard.** Killing the job is immediate and as safe as a
   graceful cancel; the graceful one costs up to 14 s for nothing.
2. **A cancelled build costs nothing extra.** The next build is correct and not
   slower; ADR-0003's "cancel on supersede" is safe for .NET and Angular builds.
3. **Build servers must outlive a single build**, or every build is twice as
   slow. They must still die with the daemon (ADR-0009). How to get both is
   open: servers are started by the build and inherit its job (see ADR-0009,
   consequence added 2026-10-14).
4. **A build is done when its root process exits**, not when its job is empty.
