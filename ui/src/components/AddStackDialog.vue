<script setup lang="ts">
// Add a stack: the user pastes a folder (a browser never reveals a picked
// folder's absolute path). A folder with devstack.toml is registered; without
// one, the hub can write a first draft (`devstack init`), or the user hands a
// ready prompt to a coding agent that writes the file with devstack's tools.
import { computed, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { api, paths } from '@/api';
import { act } from '@/lib/actions';
import { useHub } from '@/stores/hub';
import { useUi } from '@/stores/ui';
import Modal from './Modal.vue';
import Icon from './icons/Icon.vue';

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();
const hub = useHub();
const ui = useUi();
const router = useRouter();

type Inspect = { repo: string; is_dir: boolean; has_stack_file: boolean; valid: { Ok?: null; Err?: string } | null; registered: string | null; id: string };
type InitResult = { file: string; services: string[]; compose_files: string[]; todos: number; id: string | null };

const path = ref('');
const checking = ref(false);
const info = ref<Inspect | null>(null);
const problem = ref<string | null>(null);
const busy = ref(false);
const drafted = ref<InitResult | null>(null);

watch(
  () => props.open,
  (o) => {
    if (o) {
      info.value = null;
      problem.value = null;
      drafted.value = null;
    }
  },
);

// The hub shows the operating system's folder dialog (a page cannot learn a
// folder's path). Where it cannot (no desktop), the text field stays.
const canPick = ref(true);
const picking = ref(false);
async function pick() {
  picking.value = true;
  problem.value = null;
  try {
    const res = await api().request<{ path: string | null } | { error: string }>('POST', paths.pickFolder);
    const b = typeof res.body === 'object' ? res.body : null;
    if (res.status === 501) {
      canPick.value = false;
      ui.toast('info', 'No folder dialog on this machine', 'Paste the folder path instead.');
    } else if (res.ok && b && 'path' in b) {
      if (b.path) {
        path.value = b.path;
        await check();
      }
    } else {
      problem.value = b && 'error' in b ? b.error : `HTTP ${res.status}`;
    }
  } catch {
    problem.value = 'The hub does not answer.';
  } finally {
    picking.value = false;
  }
}

async function check() {
  const p = path.value.trim();
  if (!p) return;
  checking.value = true;
  problem.value = null;
  info.value = null;
  drafted.value = null;
  try {
    const res = await api().request<Inspect | { error: string }>('GET', paths.inspect(p));
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
  void router.push({ name: 'overview', params: { stack: id } });
}

async function add() {
  busy.value = true;
  const res = await act<{ id: string; name: string }>({ label: 'Add stack', path: paths.stackAdd, body: { path: info.value!.repo }, success: (b) => `${b?.name ?? 'Stack'} added` });
  busy.value = false;
  if (res.ok && res.body) openStack(res.body.id);
}

async function draft() {
  busy.value = true;
  const res = await act<InitResult>({
    label: 'Create a draft',
    path: paths.stackInit,
    body: { path: info.value!.repo },
    success: (b) => `devstack.toml written with ${b?.todos ?? 0} TODO(s)`,
  });
  busy.value = false;
  if (res.ok && res.body) drafted.value = res.body;
}

const docs = computed(() => `https://github.com/nuetzliches/devstack/blob/v${hub.version ?? 'main'}/docs/stack-file.md`);
const prompt = computed(
  () => `Set up devstack for the repository at ${info.value?.repo ?? path.value.trim()}.

devstack (https://github.com/nuetzliches/devstack) runs a local development stack from a devstack.toml in the repository root: containers from the Compose files, native services built in the background, a change goes live once it is healthy.

1. Read the stack file reference: ${docs.value}
2. ${drafted.value ? 'A draft devstack.toml exists (from `devstack init`).' : 'Run `devstack init` in the repository for a draft (it detects Compose files, .NET, Node, Go, Python and Spring Boot apps).'} Work through every TODO in it: which containers run in development (infrastructure only; what runs natively stays out), build/output/run/watch of each service, ports (the process listens on {port}), a readiness health endpoint, the environment (from a Compose service with \`compose = {...}\`), background workers (\`workers\`), and commands such as seed or reset (with \`confirm\` when they delete data).
3. Validate with \`devstack check\` (and \`devstack check --compose\`), then \`devstack doctor\`.
4. Run \`devstack skills install\`, so later sessions have the devstack skills.
5. Start with \`devstack up\` and fix what fails. Ask me before running anything that deletes data, and before committing.`,
);

async function copyPrompt() {
  try {
    await navigator.clipboard.writeText(prompt.value);
    ui.toast('success', 'Prompt copied', 'Paste it into your coding agent, in any folder.');
  } catch {
    ui.toast('warning', 'Copy failed', 'Select the text and copy it yourself.');
  }
}
</script>

<template>
  <Modal :open="open" title="Add a stack" size="lg" testid="add-stack" @close="emit('close')">
    <p class="muted">The folder of a repository. Choose it, or paste its path. devstack looks for its <code>devstack.toml</code>.</p>
    <form class="pathrow" @submit.prevent="check">
      <button v-if="canPick" type="button" class="btn" :disabled="picking" data-testid="add-stack-pick" @click="pick">
        <span v-if="picking" class="spinner" /><Icon v-else name="folder" :size="14" /> Choose folder…
      </button>
      <input v-model="path" class="input grow" type="text" placeholder="C:\src\shop  or  /home/me/src/shop" spellcheck="false" autocomplete="off" data-testid="add-stack-path" />
      <button type="submit" class="btn btn-primary" :disabled="!path.trim() || checking" data-testid="add-stack-check">
        <span v-if="checking" class="spinner" /><Icon v-else name="search" :size="14" /> Check
      </button>
    </form>

    <p v-if="problem" class="note danger">{{ problem }}</p>

    <div v-if="state === 'missing'" class="note danger">No folder at <code>{{ info!.repo }}</code>.</div>

    <div v-else-if="state === 'registered'" class="note ok">
      This stack is in the list already.
      <button type="button" class="btn btn-sm" @click="openStack(info!.registered!)">Open it</button>
    </div>

    <div v-else-if="state === 'has-file'" class="note ok" data-testid="add-stack-found">
      <p><strong>devstack.toml found.</strong> <template v-if="info!.valid?.Err">It does not load yet: <code class="err">{{ info!.valid.Err }}</code></template></p>
      <button type="button" class="btn btn-primary" :disabled="busy || !!info!.valid?.Err" data-testid="add-stack-add" @click="add">
        <Icon name="plus" :size="14" /> Add to the list
      </button>
    </div>

    <template v-else-if="state === 'no-file'">
      <div v-if="!drafted" class="choices">
        <div class="choice">
          <h3><Icon name="file" :size="15" /> Create a draft</h3>
          <p class="muted">devstack writes a first <code>devstack.toml</code> from what it finds (Compose files, .NET, Node, Go, Python and Spring Boot apps). Every guess is marked <code>TODO</code>; nothing else in the repository changes.</p>
          <button type="button" class="btn btn-primary" :disabled="busy" data-testid="add-stack-init" @click="draft">
            <span v-if="busy" class="spinner" /><Icon v-else name="play" :size="14" /> Create draft
          </button>
        </div>
        <div class="choice">
          <h3><Icon name="terminal" :size="15" /> Let a coding agent do it</h3>
          <p class="muted">A prompt for Claude Code, Codex or similar: it runs <code>devstack init</code>, works through the TODOs with the reference, validates and starts the stack.</p>
          <button type="button" class="btn" data-testid="add-stack-prompt" @click="copyPrompt"><Icon name="copy" :size="14" /> Copy prompt</button>
        </div>
      </div>
      <div v-else class="note ok" data-testid="add-stack-drafted">
        <p>
          <strong>Draft written:</strong> <code>{{ drafted.file }}</code>, {{ drafted.services.length }} service(s)<template v-if="drafted.services.length"> ({{ drafted.services.join(', ') }})</template>,
          {{ drafted.compose_files.length }} Compose file(s), <strong>{{ drafted.todos }} TODO(s)</strong> to work through.
        </p>
        <p class="muted">Then <code>devstack check</code> and <code>devstack up</code> in the folder<template v-if="!drafted.id">; the stack appears here once its file loads</template>.</p>
        <div class="row">
          <button type="button" class="btn" @click="copyPrompt"><Icon name="copy" :size="14" /> Copy a prompt to finish it with an agent</button>
          <button v-if="drafted.id" type="button" class="btn btn-primary" @click="openStack(drafted.id)">Open the stack</button>
        </div>
      </div>
      <details class="prompt">
        <summary>Show the prompt</summary>
        <pre>{{ prompt }}</pre>
      </details>
    </template>
  </Modal>
</template>

<style scoped>
.pathrow {
  display: flex;
  gap: 8px;
  margin: 12px 0;
}
.input {
  font: 13px/1.4 var(--font-mono, ui-monospace, Consolas, monospace);
  padding: 7px 10px;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface, transparent);
  color: inherit;
  min-width: 0;
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
.prompt {
  margin-top: 12px;
}
.prompt pre {
  white-space: pre-wrap;
  font-size: 12px;
  max-height: 240px;
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 10px;
}
</style>
