<script setup lang="ts">
// The store of what `prepare` makes (ADR-0062): one per repository and
// machine, the main tree's. A card beside the lane with its entries and a
// remove action per entry. The next `prepare` of an entry's inputs runs and
// makes it again.
import { computed, onMounted, ref, watch } from 'vue';
import { api, paths, v1, Unreachable } from '@/api';
import type { StoreEntry, StoreView } from '@/api/types';
import { act } from '@/lib/actions';
import { ago, bytes, dateTime, useNow } from '@/lib/format';
import { shortTime, storeLine, treeLabel } from '@/lib/store';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const stack = useStack();
const now = useNow();
const view = ref<StoreView | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);
const removing = ref<string | null>(null);

/** The main tree's daemon owns the store: asked there, whichever lane is shown. */
const path = (p: string) => (stack.id ? paths.api(stack.id, p, null) : null);

async function load() {
  const p = path(v1.store);
  if (!p) return;
  loading.value = true;
  try {
    const res = await api().request<StoreView>('GET', p);
    if (res.ok && res.body && typeof res.body === 'object') {
      view.value = res.body;
      error.value = null;
    } else {
      // An older daemon has no store to show (404), a stopped stack none (503).
      view.value = null;
      error.value = res.status === 404 ? null : res.status === 503 ? 'The stack is not running.' : `HTTP ${res.status}`;
    }
  } catch (e) {
    view.value = null;
    error.value = e instanceof Unreachable ? 'The hub is not reachable.' : String(e);
  } finally {
    loading.value = false;
  }
}

const id = (e: StoreEntry) => `${e.service}/${e.key}`;

async function remove(e: StoreEntry) {
  const p = path(v1.storeEntry(e.service, e.key));
  if (!p) return;
  removing.value = id(e);
  try {
    const r = await act({
      label: `Remove ${e.service} ${e.key.slice(0, 8)} from the store`,
      method: 'DELETE',
      path: p,
      confirmText: `${bytes(e.bytes)} of ${e.outputs.join(', ')} go. The next \`prepare\` of these inputs runs again and makes it anew.`,
      confirmLabel: 'Remove',
      success: `${e.service}: store entry removed`,
    });
    if (r.ok) await load();
  } finally {
    removing.value = null;
  }
}

onMounted(load);
watch(
  () => stack.id,
  () => void load(),
);
const entries = computed(() => view.value?.entries ?? []);
const canRemove = computed(() => !!view.value?.owned && !stack.readOnly);
</script>

<template>
  <section v-if="view" class="card" data-testid="store-card">
    <header class="card-header">
      <Icon name="hardDrive" :size="16" class="muted" />
      <h2>Store</h2>
      <span class="count tabular" data-testid="store-summary">{{ storeLine(view) }}</span>
      <span class="grow" />
      <button type="button" class="btn btn-ghost btn-sm btn-icon" :disabled="loading" title="Reload" aria-label="Reload the store" @click="load"><Icon name="restart" /></button>
    </header>
    <p class="card-body hint muted small">
      What <code>prepare</code> made (its <code>outputs</code>), by its inputs, this machine's platform and its <code>version</code>: a lane or a build with the same
      inputs copies it instead of running <code>prepare</code>. An entry not used for 14 days goes by itself.
    </p>
    <div v-if="!entries.length" class="card-body faint small">The store is empty.</div>
    <div v-else class="table-wrap">
      <table class="table" data-testid="store-table">
        <thead>
          <tr>
            <th>Service</th>
            <th>Outputs</th>
            <th class="num">Size</th>
            <th>Last used</th>
            <th v-if="canRemove" />
          </tr>
        </thead>
        <tbody>
          <tr v-for="e in entries" :key="id(e)" :data-entry="id(e)">
            <td>
              <span class="strong">{{ e.service }}</span>
              <span class="mono faint key" :title="e.key">{{ e.key.slice(0, 8) }}</span>
            </td>
            <td class="mono small outputs">{{ e.outputs.join(', ') }}</td>
            <td class="num tabular" :title="`${e.files.toLocaleString()} files`">{{ bytes(e.bytes) }}</td>
            <td class="small">
              <span :title="dateTime(e.last_used)">{{ ago(e.last_used, now) }}</span>
              <span class="faint sub" :title="dateTime(e.created)">
                made {{ ago(e.created, now) }} · prepare {{ shortTime(e.prepare_ms) }}<template v-if="e.copy_ms !== undefined">, copy {{ shortTime(e.copy_ms) }}</template>
              </span>
              <span v-if="e.used_by.length" class="faint sub">used by {{ e.used_by.map(treeLabel).join(', ') }}</span>
            </td>
            <td v-if="canRemove" class="num">
              <button
                type="button"
                class="btn btn-ghost btn-sm btn-icon"
                :disabled="removing === id(e)"
                :data-testid="`store-remove-${id(e)}`"
                title="Remove this entry"
                :aria-label="`Remove ${e.service} ${e.key.slice(0, 8)} from the store`"
                @click="remove(e)"
              >
                <span v-if="removing === id(e)" class="spinner" /><Icon v-else name="trash" :size="14" />
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p v-if="!view.owned" class="card-body faint small">This is the store of the node the lane runs on, read only here: the node removes its entries.</p>
  </section>
  <p v-else-if="error" class="faint small">Store: {{ error }}</p>
</template>

<style scoped>
.hint {
  margin: 0;
  border-bottom: 1px solid var(--border);
}
.strong {
  font-weight: 600;
}
.key {
  margin-left: 6px;
  font-size: 11.5px;
}
.outputs {
  word-break: break-all;
}
.sub {
  display: block;
  font-size: 11.5px;
}
.small {
  font-size: 12px;
}
</style>
