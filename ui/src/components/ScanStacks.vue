<script setup lang="ts">
// Find every project with a devstack.toml below one folder (the one all
// repositories live in, e.g. D:\Sources) and add the ones chosen (all new ones
// are, at first). Repositories are looked at at their root only; linked
// worktrees are skipped (they run as lanes of their main tree).
import { computed, ref } from 'vue';
import { api, paths } from '@/api';
import { useUi } from '@/stores/ui';
import FolderField from './FolderField.vue';
import Icon from './icons/Icon.vue';

const emit = defineEmits<{ added: [ids: string[]] }>();
const ui = useUi();

type Found = { repo: string; id: string; name: string | null; error: string | null; registered: boolean; added: boolean };

const path = ref('');
const scanning = ref(false);
const adding = ref(false);
const found = ref<Found[] | null>(null);
const problem = ref<string | null>(null);

const fresh = computed(() => (found.value ?? []).filter((f) => !f.registered && !f.error));
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
    const res = await api().request<Found[] | { error: string }>('POST', paths.scanStacks, { path: p });
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
    <p v-if="problem" class="note danger">{{ problem }}</p>

    <template v-if="found">
      <p v-if="!found.length" class="muted">No repository with a <code>devstack.toml</code> below this folder (three levels deep).</p>
      <div v-else class="list">
        <label v-if="fresh.length > 1" class="item head">
          <input type="checkbox" :checked="all" :indeterminate="some && !all" data-testid="scan-all" @change="toggleAll" />
          <span class="grow">{{ count }} of {{ fresh.length }} new chosen</span>
        </label>
        <component :is="fresh.includes(f) ? 'label' : 'div'" v-for="f in found" :key="f.id" class="item" :class="{ off: !fresh.includes(f) }" :data-testid="`scan-found-${f.id}`">
          <input v-if="fresh.includes(f)" type="checkbox" :checked="chosen.has(f.id)" @change="toggle(f.id)" />
          <Icon v-else :name="f.error ? 'alert' : 'checkCircle'" :size="15" :class="f.error ? 'err' : 'ok'" />
          <div class="grow">
            <div class="name">{{ f.name ?? f.repo.split(/[\\/]/).pop() }}</div>
            <div class="repo mono faint">{{ f.repo }}</div>
            <div v-if="f.error" class="err small">{{ f.error.split('\n')[0] }}</div>
          </div>
          <span class="tag">{{ f.error ? 'does not load' : f.added ? 'added' : f.registered ? 'in the list' : 'new' }}</span>
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
