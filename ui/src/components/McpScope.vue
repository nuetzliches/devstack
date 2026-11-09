<script setup lang="ts">
// Where devstack's MCP server is registered for each coding agent (#492):
// per repository (the default: the stack's head offers a prompt for the
// repository's own file), or for you, in every folder: the hub writes one
// entry "devstack" into the agent's own configuration, and takes it out
// again when switched back. Your choice, on this machine.
import { onMounted, ref } from 'vue';
import { api, paths } from '@/api';
import type { McpScope, McpScopeAgent, McpScopeView } from '@/api/types';
import { act } from '@/lib/actions';

const state = ref<McpScopeView | null>(null);
const problem = ref<string | null>(null);
const busy = ref<string | null>(null);

onMounted(async () => {
  try {
    const r = await api().request<McpScopeView>('GET', paths.mcpScope);
    if (r.ok && r.body && typeof r.body === 'object' && Array.isArray(r.body.agents)) state.value = r.body;
    else problem.value = r.status === 404 ? null : `The hub answered ${r.status}.`;
  } catch {
    problem.value = 'The hub does not answer.';
  }
});

async function set(a: McpScopeAgent, scope: McpScope) {
  busy.value = a.id;
  const r = await act<McpScopeView>({
    label: scope === 'user' ? `devstack's MCP server for ${a.name} in every folder` : `devstack's MCP server for ${a.name} per repository`,
    method: 'PUT',
    path: paths.mcpScope,
    body: { agents: { [a.id]: scope } },
    success: false,
  });
  busy.value = null;
  if (r.ok && r.body && typeof r.body === 'object') state.value = r.body;
}
</script>

<template>
  <section v-if="state" class="card" data-testid="mcp-scope">
    <header class="card-header">
      <h2>devstack's MCP server for your agents</h2>
    </header>
    <div class="card-body">
      <p class="muted">
        Per repository, an agent finds devstack's tools (and the upstreams above) where the repository's own file registers them; the stack's head offers to add
        it. For you, in every folder, the hub writes one entry <code>devstack</code> into the agent's own configuration, for every folder you work in,
        repositories without a stack included, and takes it out again when you switch back. The rest of that file stays as it is.
      </p>
      <p v-if="problem" class="err">{{ problem }}</p>
      <ul class="agents">
        <li v-for="a in state.agents" :key="a.id" class="agent" :data-testid="`mcp-scope-${a.id}`">
          <div class="who">
            <strong>{{ a.name }}</strong>
            <span class="faint small">{{ a.installed ? a.file : 'not installed here' }}</span>
            <span v-if="a.registered && a.scope === 'repository'" class="small muted">its configuration registers it for every folder already</span>
            <span v-if="a.error" class="small err" data-testid="mcp-scope-error">{{ a.error }}</span>
          </div>
          <div class="btn-group" role="group" :aria-label="`devstack's MCP server for ${a.name}`">
            <button type="button" class="chip-toggle" :aria-pressed="a.scope === 'repository'" :disabled="busy === a.id" :data-testid="`mcp-scope-${a.id}-repository`" @click="set(a, 'repository')">
              Per repository
            </button>
            <button
              type="button"
              class="chip-toggle"
              :aria-pressed="a.scope === 'user'"
              :disabled="busy === a.id || !a.installed"
              :title="a.installed ? '' : 'The agent is not installed on this machine'"
              :data-testid="`mcp-scope-${a.id}-user`"
              @click="set(a, 'user')"
            >
              In every folder
            </button>
          </div>
        </li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.agents {
  list-style: none;
  margin: 8px 0 0;
  padding: 0;
}
.agent {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 0;
  border-top: 1px solid var(--border);
}
.who {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.who span {
  overflow-wrap: anywhere;
}
.err {
  color: var(--danger-text);
}
</style>
