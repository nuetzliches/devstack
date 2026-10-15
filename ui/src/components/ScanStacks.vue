<script setup lang="ts">
// Find every project with a devstack.toml below one folder (the one all
// repositories live in, e.g. D:\Sources) and add them at once. Repositories
// are looked at at their root only; linked worktrees are skipped (they run as
// lanes of their main tree).
import { computed, ref } from 'vue';
import { api, paths } from '@/api';
import { useUi } from '@/stores/ui';
import Icon from './icons/Icon.vue';

const emit = defineEmits<{ added: [ids: string[]] }>();
const ui = useUi();

type Found = { repo: string; id: string; name: string | null; error: string | null; registered: boolean; added: boolean };

const path = ref('');
const scanning = ref(false);
const adding = ref(false);
const found = ref<Found[] | null>(null);
const problem = ref<string | null>(null);
const canPick = ref(true);

const fresh = computed(() => (found.value ?? []).filter((f) => !f.registered && !f.error));

async function scan(add = false) {
  const p = path.value.trim();
  if (!p) return;
  if (add) adding.value = true;
  else scanning.value = true;
  problem.value = null;
  try {
    const res = await api().request<Found[] | { error: string }>('POST', paths.scanStacks, { path: p, add });
    const b = res.body;
    if (res.ok && Array.isArray(b)) {
      found.value = b;
      if (add) {
        const ids = b.filter((f) => f.added).map((f) => f.id);
        ui.toast('success', `${ids.length} stack${ids.length === 1 ? '' : 's'} added`, 'Start them from the list.');
        emit('added', ids);
      }
    } else problem.value = b && typeof b === 'object' && 'error' in b ? b.error : `HTTP ${res.status}`;
  } catch {
    problem.value = 'The hub does not answer.';
  } finally {
    scanning.value = false;
    adding.value = false;
  }
}

async function pick() {
  try {
    const res = await api().request<{ path: string | null }>('POST', paths.pickFolder);
    if (res.status === 501) {
      canPick.value = false;
      return;
    }
    if (res.ok && res.body && typeof res.body === 'object' && res.body.path) {
      path.value = res.body.path;
      await scan();
    }
  } catch {
    problem.value = 'The hub does not answer.';
  }
}
</script>

<template>
  <div class="scan" data-testid="scan-stacks">
    <form class="pathrow" @submit.prevent="scan()">
      <button v-if="canPick" type="button" class="btn" data-testid="scan-pick" @click="pick"><Icon name="folder" :size="14" /> Choose folder…</button>
      <input v-model="path" class="input grow" type="text" placeholder="D:\Sources  or  ~/src" spellcheck="false" autocomplete="off" data-testid="scan-path" />
      <button type="submit" class="btn btn-primary" :disabled="!path.trim() || scanning" data-testid="scan-run">
        <span v-if="scanning" class="spinner" /><Icon v-else name="search" :size="14" /> Scan
      </button>
    </form>
    <p v-if="problem" class="note danger">{{ problem }}</p>

    <template v-if="found">
      <p v-if="!found.length" class="muted">No repository with a <code>devstack.toml</code> below this folder (three levels deep).</p>
      <ul v-else class="list">
        <li v-for="f in found" :key="f.id" class="item" :data-testid="`scan-found-${f.id}`">
          <Icon :name="f.error ? 'alert' : f.registered ? 'checkCircle' : 'plus'" :size="15" :class="f.error ? 'err' : f.registered ? 'ok' : 'muted'" />
          <div class="grow">
            <div class="name">{{ f.name ?? f.repo.split(/[\\/]/).pop() }}</div>
            <div class="repo mono faint">{{ f.repo }}</div>
            <div v-if="f.error" class="err small">{{ f.error.split('\n')[0] }}</div>
          </div>
          <span class="tag">{{ f.error ? 'does not load' : f.added ? 'added' : f.registered ? 'in the list' : 'new' }}</span>
        </li>
      </ul>
      <div v-if="fresh.length" class="row">
        <button type="button" class="btn btn-primary" :disabled="adding" data-testid="scan-add" @click="scan(true)">
          <span v-if="adding" class="spinner" /><Icon v-else name="plus" :size="14" /> Add {{ fresh.length }} new stack{{ fresh.length === 1 ? '' : 's' }}
        </button>
      </div>
    </template>
  </div>
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
