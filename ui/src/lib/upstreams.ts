import type { AgentOwnServer } from '@/api/types';

/** Where an agent has a pool's upstream as a server of its own, in a line:
 *  the agent, the scope (and folder), the entry's name. */
export function ownServerWhere(d: AgentOwnServer): string {
  const scope = d.scope === 'user' ? 'for you, in every folder' : d.scope === 'local' ? `for you, in ${d.folder}` : `in ${d.folder}/.mcp.json`;
  return `Already set up directly in ${d.agent_name} (${scope}, as “${d.name}”).`;
}

/** What having it twice means, by what the user made of the pool's offer. */
export function ownServerAdvice(status: string): string {
  return status === 'accepted'
    ? 'The agent now gets its tools twice. Remove the own entry:'
    : 'To have it once, through devstack: use it here, then remove the own entry:';
}
