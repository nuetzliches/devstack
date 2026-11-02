<script setup lang="ts">
// Add a stack: the user pastes a folder (a browser never reveals a picked
// folder's absolute path). A folder with devstack.toml is registered; without
// one, a coding agent writes it with devstack's tools (`devstack init`, then
// the TODOs), from the hub's prompt (copied, or the agent started with it); a
// file that does not load, the same for repairing it. The dashboard writes
// no file of the repository itself (ADR-0071).
import { computed, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { api, paths } from '@/api';
import { act } from '@/lib/actions';
import AgentPrompt from './AgentPrompt.vue';
import FolderField from './FolderField.vue';
import Modal from './Modal.vue';
import Icon from './icons/Icon.vue';
import ScanStacks from './ScanStacks.vue';
import { stackPage } from '@/lib/stackRoutes';

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();
const router = useRouter();

type Inspect = { repo: string; is_dir: boolean; has_stack_file: boolean; valid: { Ok?: null; Err?: string } | null; registered: string | null; id: string };

/** One folder, or every project below a folder. */
const mode = ref<'one' | 'scan'>('one');
const path = ref('');
const checking = ref(false);
const info = ref<Inspect | null>(null);
const problem = ref<string | null>(null);
const busy = ref(false);

watch(
  () => props.open,
  (o) => {
    if (o) {
      info.value = null;
      problem.value = null;
    }
  },
);

// A typed or pasted path is checked once typing pauses; a picked one, Enter
// or a click at once. An answer for a path the field no longer holds is
// dropped.
let timer: ReturnType<typeof setTimeout> | undefined;
watch(
  path,
  () => {
    clearTimeout(timer);
    info.value = null;
    problem.value = null;
    if (path.value.trim()) timer = setTimeout(check, 450);
  },
  { flush: 'sync' },
);

async function check() {
  clearTimeout(timer);
  const p = path.value.trim();
  if (!p) return;
  checking.value = true;
  problem.value = null;
  info.value = null;
  try {
    const res = await api().request<Inspect | { error: string }>('GET', paths.inspect(p));
    if (p !== path.value.trim()) return;
    const b = typeof res.body === 'object' ? res.body : null;
    if (res.ok && b && 'repo' in b) info.value = b;
    else problem.value = b && 'error' in b ? b.error : `HTTP ${res.status}`;
  } catch {
    problem.value = 'The hub does not answer.';
  } finally {
    checking.value = false;
  }
}

const state = computed(() => {
  const i = info.value;
  if (!i) return null;
  if (!i.is_dir) return 'missing';
  if (i.registered) return 'registered';
  if (i.has_stack_file) return 'has-file';
  return 'no-file';
});

function openStack(id: string) {
  emit('close');
  void router.push(stackPage(id));
}

async function add() {
  busy.value = true;
  const res = await act<{ id: string; name: string }>({ label: 'Add stack', path: paths.stackAdd, body: { path: info.value!.repo }, success: (b) => `${b?.name ?? 'Stack'} added` });
  busy.value = false;
  if (res.ok && res.body) openStack(res.body.id);
}

</script>

<template>
  <Modal :open="open" title="Add a stack" size="lg" testid="add-stack" @close="emit('close')">
    <div class="tabs" role="tablist" aria-label="How to add">
      <button type="button" role="tab" class="tab" :aria-selected="mode === 'one'" :class="{ active: mode === 'one' }" data-testid="add-mode-one" @click="mode = 'one'">
        <Icon name="folder" :size="14" /> One repository
      </button>
      <button type="button" role="tab" class="tab" :aria-selected="mode === 'scan'" :class="{ active: mode === 'scan' }" data-testid="add-mode-scan" @click="mode = 'scan'">
        <Icon name="search" :size="14" /> Scan a folder of repositories
      </button>
    </div>
    <template v-if="mode === 'scan'">
      <p class="muted">The folder your repositories live in, e.g. <code>D:\Sources</code>. Every repository with a <code>devstack.toml</code> at its root is found (three levels deep); worktrees run as lanes of their main tree.</p>
      <ScanStacks @added="emit('close')" />
    </template>
    <template v-else>
    <p class="muted">The folder of a repository: browse to it, or paste its path. devstack looks for its <code>devstack.toml</code>.</p>
    <FolderField v-model="path" placeholder="C:\src\shop  or  /home/me/src/shop" :busy="checking" testid="add-stack" @submit="check" @picked="check" @problem="problem = $event" />

    <p v-if="problem" class="note danger">{{ problem }}</p>

    <div v-if="state === 'missing'" class="note danger">No folder at <code>{{ info!.repo }}</code>.</div>

    <div v-else-if="state === 'registered'" class="note ok">
      This stack is in the list already.
      <button type="button" class="btn btn-sm" @click="openStack(info!.registered!)">Open it</button>
    </div>

    <div v-else-if="state === 'has-file'" class="note ok" data-testid="add-stack-found">
      <p><strong>devstack.toml found.</strong> <template v-if="info!.valid?.Err">It does not load yet: <code class="err">{{ info!.valid.Err }}</code></template></p>
      <button v-if="!info!.valid?.Err" type="button" class="btn btn-primary" :disabled="busy" data-testid="add-stack-add" @click="add">
        <Icon name="plus" :size="14" /> Add to the list
      </button>
      <template v-else>
        <p class="muted">A coding agent can repair it with the reference; then check again.</p>
        <AgentPrompt kind="fix-file" :target="{ path: info!.repo }" />
      </template>
    </div>

    <template v-else-if="state === 'no-file'">
      <div class="choice" data-testid="add-stack-setup">
        <h3><Icon name="terminal" :size="15" /> Let a coding agent set it up</h3>
        <p class="muted">A prompt for Claude Code, Codex or similar: it runs <code>devstack init</code> for a first draft (Compose files, .NET, Node, Go, Python and Spring Boot apps), works through its TODOs with the reference, validates and starts the stack. Without an agent, copy the prompt, or run <code>devstack init</code> in the folder yourself.</p>
        <AgentPrompt kind="setup" :target="{ path: info!.repo }" />
      </div>
    </template>
    </template>
  </Modal>
</template>

<style scoped>
.tabs {
  display: flex;
  gap: 2px;
  margin: 0 0 12px;
  border-bottom: 1px solid var(--border);
}
.tab {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  height: 38px;
  padding: 0 12px;
  margin-bottom: -1px;
  border: 0;
  border-bottom: 2px solid transparent;
  background: none;
  color: var(--text-muted);
  font-weight: 550;
  font-size: 13px;
  cursor: pointer;
}
.tab:hover {
  color: var(--text);
}
.tab.active {
  color: var(--text);
  border-bottom-color: var(--accent);
}
.tab:focus-visible {
  outline: none;
  box-shadow: inset var(--focus);
}
.note {
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 12px 14px;
  margin-top: 8px;
}
.note p {
  margin: 0 0 8px;
}
.note.danger {
  border-color: var(--danger, #d14);
}
.note.ok {
  border-color: var(--accent);
}
.err {
  white-space: pre-wrap;
}
.choices {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: 12px;
  margin-top: 8px;
}
.choice {
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 12px 14px;
}
.choice h3 {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 14px;
  margin: 0 0 6px;
}
.row {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
</style>
