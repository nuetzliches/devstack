// Prompts the dashboard hands to a coding agent (Claude Code, Codex or
// similar): the user copies one and pastes it into the agent, in any folder.

const REPO_URL = 'https://github.com/nuetzliches/devstack';

/**
 * Bring a repository set up with an older devstack up to this one: what
 * changed in between, the stack file against the reference, the generated
 * skills refreshed, the stack started again.
 */
export function upgradePrompt(o: { repo: string; file: string; from: string | null; current: string }): string {
  const at = `${REPO_URL}/blob/v${o.current}`;
  const since = o.from ? `devstack ${o.from}` : 'an earlier devstack';
  const changes = o.from
    ? `the sections after ${o.from}, up to ${o.current}, in ${at}/CHANGELOG.md`
    : `the newest sections of ${at}/CHANGELOG.md`;
  return `Bring the devstack setup of the repository at ${o.repo} up to devstack ${o.current}. It was last set up with ${since} (the version its generated agent skills name).

devstack (${REPO_URL}) runs a local development stack from ${o.file}: containers from the Compose files, native services built in the background, a change goes live once it is healthy.

1. Read what changed since then: ${changes}. Note what concerns this repository: keys that changed or went away, new defaults, new features.
2. Hold ${o.file} against the stack file reference of ${o.current}: ${at}/docs/stack-file.md
3. Run \`devstack check\` (and \`devstack check --compose\`) and \`devstack doctor\`, and fix every error and warning.
4. Tell me which new features would help this stack, and add only those I agree to. Raise \`[stack] requires\` only when the file uses something an older devstack does not know: it shuts out everyone still on one.
5. Run \`devstack skills install\` to refresh the generated skills. Check the project's own skills, CLAUDE.md and AGENTS.md for anything that contradicts devstack ${o.current}.
6. Restart the stack (\`devstack down\`, then \`devstack up\`) and check that every service becomes healthy. Ask me before running anything that deletes data, and before committing.`;
}
