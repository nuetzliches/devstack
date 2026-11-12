<script setup lang="ts">
// Find the projects below one folder (the one all repositories live in, e.g.
// D:\Sources) and add the ones chosen (all new ones are, at first). By default
// the git repositories with a devstack.toml one level down; one without the
// file is added "not set up", and a coding agent sets it up (ADR-0089).
// Repositories are looked at at their root only; linked worktrees are skipped
// (they run as lanes of their main tree).
// The folder is suggested from the stacks already in the list: the folder
// they sit in fills the empty field, the others (and that one again) are
// chips under it. Nothing is scanned until the user asks.
import { computed, ref, watch } from 'vue';
import { api, paths } from '@/api';
import { suggestRoots } from '@/lib/scanRoots';
import { useHub } from '@/stores/hub';
import { useUi } from '@/stores/ui';
import FolderField from './FolderField.vue';
import Icon from './icons/Icon.vue';

const emit = defineEmits<{ added: [ids: string[]] }>();
const ui = useUi();
const hub = useHub();

type Found = { repo: string; id: string; name: string | null; error: string | null; configured: boolean; registered: boolean; added: boolean };

const path = ref('');
const scanning = ref(false);
const adding = ref(false);
const found = ref<Found[] | null>(null);
const problem = ref<string | null>(null);
/** What the scan lists: one of the first two is always on. */
const gitOnly = ref(true);
const configuredOnly = ref(true);
const deep = ref(false);

/** The folders the registered stacks sit in, the one with the most first. */
const roots = computed(() => suggestRoots(hub.stacks.map((s) => s.repo)));

// The first one fills the field once, when it is empty: a path the user typed
// or picked is never overwritten, and a list that arrives late still fills it.
let prefilled = false;
watch(
  roots,
  (r) => {
    if (prefilled || !r.length) return;
    prefilled = true;
    if (!path.value.trim()) path.value = r[0].path;
  },
  { immediate: true },
);

function pickRoot(root: string) {
  path.value = root;
  void scan();
}

const fresh = computed(() => (found.value ?? []).filter((f) => !f.registered && !f.error));
/** What the list shows: what is not in the stacks list yet on top (new ones,
 * those that do not load, those just added here, so a row stays where it was
 * when it is added), the ones already in it below; the order of the scan
 * within each part. */
const ordered = computed(() => {
  const rank = (f: Found) => (f.registered && !f.added ? 1 : 0);
  return [...(found.value ?? [])].sort((a, b) => rank(a) - rank(b));
});
/** The new repositories to add, by id: every new one after a scan. */
const chosen = ref(new Set<string>());
const all = computed(() => fresh.value.length > 0 && fresh.value.every((f) => chosen.value.has(f.id)));
const some = computed(() => fresh.value.some((f) => chosen.value.has(f.id)));
const count = computed(() => fresh.value.filter((f) => chosen.value.has(f.id)).length);

function toggle(id: string) {
  const next = new Set(chosen.value);
  if (!next.delete(id)) next.add(id);
  chosen.value = next;
}

function toggleAll() {
  chosen.value = new Set(all.value ? [] : fresh.value.map((f) => f.id));
}

async function scan() {
  const p = path.value.trim();
  if (!p) return;
  scanning.value = true;
  problem.value = null;
  try {
    const res = await api().request<Found[] | { error: string }>('POST', paths.scanStacks, {
      path: p,
      git_only: gitOnly.value,
      configured_only: configuredOnly.value,
      depth: deep.value ? 3 : 1,
    });
    const b = res.body;
    if (res.ok && Array.isArray(b)) {
      found.value = b;
      chosen.value = new Set(fresh.value.map((f) => f.id));
    } else problem.value = b && typeof b === 'object' && 'error' in b ? b.error : `HTTP ${res.status}`;
  } catch {
    problem.value = 'The hub does not answer.';
  } finally {
    scanning.value = false;
  }
}

// A changed filter scans again what was scanned.
watch([gitOnly, configuredOnly, deep], () => {
  if (found.value) void scan();
});

// One registration each, as a single repository is added; what fails stays in
// the list with its reason, the rest are added.
async function add() {
  adding.value = true;
  problem.value = null;
  const ids: string[] = [];
  for (const f of fresh.value.filter((f) => chosen.value.has(f.id))) {
    try {
      const res = await api().request<{ id: string } | { error: string }>('POST', paths.stackAdd, { path: f.repo });
      const b = typeof res.body === 'object' ? res.body : null;
      if (res.ok && b && 'id' in b) {
        f.registered = true;
        f.added = true;
        ids.push(b.id);
      } else f.error = b && 'error' in b ? b.error : `HTTP ${res.status}`;
    } catch {
      f.error = 'The hub does not answer.';
      break;
    }
  }
  adding.value = false;
  if (!ids.length) return;
  ui.toast('success', `${ids.length} stack${ids.length === 1 ? '' : 's'} added`, 'Start them from the list.');
  if (!found.value?.some((f) => f.error && chosen.value.has(f.id))) emit('added', ids);
}
</script>

<template>
  <div class="scan" data-testid="scan-stacks">
    <FolderField v-model="path" placeholder="D:\Sources  or  ~/src" testid="scan" @submit="scan()" @picked="scan()" @problem="problem = $event">
      <button type="submit" class="btn btn-primary" :disabled="!path.trim() || scanning" data-testid="scan-run">
        <span v-if="scanning" class="spinner" /><Icon v-else name="search" :size="14" /> Scan
      </button>
    </FolderField>
    <p v-if="roots.length" class="roots" data-testid="scan-roots">
      <span class="muted">Where your stacks live:</span>
      <button
        v-for="(r, i) in roots"
        :key="r.path"
        type="button"
        class="chip"
        :class="{ active: path.trim() === r.path }"
        :aria-pressed="path.trim() === r.path"
        :title="`${r.count} stack${r.count === 1 ? '' : 's'} in this folder`"
        :data-testid="`scan-root-${i}`"
        @click="pickRoot(r.path)"
      >
        <span class="mono">{{ r.path }}</span> <span class="faint">{{ r.count }}</span>
      </button>
    </p>
    <div class="filters" data-testid="scan-filters">
      <label title="A plain folder with a devstack.toml is listed too when this is off">
        <input v-model="gitOnly" type="checkbox" :disabled="!configuredOnly" data-testid="scan-git-only" /> Only git repositories
      </label>
      <label title="A repository without one is added as not set up; a coding agent sets it up">
        <input v-model="configuredOnly" type="checkbox" :disabled="!gitOnly" data-testid="scan-configured-only" /> Only configured (<code>devstack.toml</code> exists)
      </label>
      <label title="For folders grouped by customer or organisation, e.g. ~/src/github.com/org/repo">
        <input v-model="deep" type="checkbox" data-testid="scan-deep" /> Search subfolders (three levels)
      </label>
    </div>
    <p v-if="problem" class="note danger">{{ problem }}</p>

    <template v-if="found">
      <p v-if="!found.length" class="muted">
        No {{ gitOnly ? 'repository' : 'folder' }}{{ configuredOnly ? ' with a devstack.toml' : '' }} {{ deep ? 'below this folder (three levels deep)' : 'in this folder' }}.
      </p>
      <div v-else class="list">
        <label v-if="fresh.length > 1" class="item head">
          <input type="checkbox" :checked="all" :indeterminate="some && !all" data-testid="scan-all" @change="toggleAll" />
          <span class="grow">{{ count }} of {{ fresh.length }} new chosen</span>
        </label>
        <component :is="fresh.includes(f) ? 'label' : 'div'" v-for="f in ordered" :key="f.id" class="item" :class="{ off: !fresh.includes(f) }" :data-testid="`scan-found-${f.id}`">
          <input v-if="fresh.includes(f)" type="checkbox" :checked="chosen.has(f.id)" @change="toggle(f.id)" />
          <Icon v-else :name="f.error ? 'alert' : 'checkCircle'" :size="15" :class="f.error ? 'err' : 'ok'" />
          <div class="grow">
            <div class="name">{{ f.name ?? f.repo.split(/[\\/]/).pop() }}</div>
            <div class="repo mono faint">{{ f.repo }}</div>
            <div v-if="f.error" class="err small">{{ f.error.split('\n')[0] }}</div>
          </div>
          <span class="tag" :class="{ unset: !f.configured && !f.registered }">{{ f.error ? 'does not load' : f.added ? 'added' : f.registered ? 'in the list' : f.configured ? 'new' : 'not set up' }}</span>
        </component>
      </div>
      <div v-if="fresh.length" class="row">
        <button type="button" class="btn btn-primary" :disabled="adding || !count" data-testid="scan-add" @click="add">
          <span v-if="adding" class="spinner" /><Icon v-else name="plus" :size="14" /> Add {{ count }} stack{{ count === 1 ? '' : 's' }}
        </button>
      </div>
    </template>
  </div>
</template>

<style scoped>
.roots {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  margin: 0 0 4px;
  font-size: 12px;
}
.chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 2px 10px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: transparent;
  color: var(--text);
  font-size: 12px;
  cursor: pointer;
}
.chip:hover {
  background: var(--surface-hover);
}
.chip.active {
  border-color: var(--border-strong);
  background: var(--surface-hover);
}
.chip:focus-visible {
  outline: none;
  box-shadow: var(--focus);
}
.filters {
  display: flex;
  flex-wrap: wrap;
  gap: 6px 18px;
  margin: 10px 0;
  font-size: 13px;
}
.filters label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;
}
.filters input {
  margin: 0;
  accent-color: var(--accent);
}
.tag.unset {
  color: var(--warn);
}
.list {
  list-style: none;
  margin: 0 0 12px;
  padding: 0;
  border: 1px solid var(--border);
  border-radius: 10px;
  max-height: 340px;
  overflow: auto;
}
.item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 12px;
  border-bottom: 1px solid var(--border);
}
.item:last-child {
  border-bottom: 0;
}
label.item {
  cursor: pointer;
}
label.item:hover {
  background: var(--surface-hover);
}
.item input {
  flex: none;
  width: 15px;
  height: 15px;
  margin: 0;
  accent-color: var(--accent);
}
.head {
  position: sticky;
  top: 0;
  z-index: 1;
  background: var(--surface-2);
  font-size: 12px;
  color: var(--text-muted);
}
.off .name {
  color: var(--text-muted);
}
.name {
  font-weight: 600;
}
.repo {
  font-size: 12px;
  word-break: break-all;
}
.small {
  font-size: 12px;
}
.err {
  color: var(--danger);
}
.ok {
  color: var(--ok);
}
.note.danger {
  border: 1px solid var(--danger);
  border-radius: 10px;
  padding: 10px 12px;
}
.row {
  display: flex;
  gap: 8px;
}
</style>
