<script setup lang="ts">
// The store of what `prepare` makes (ADR-0062): one per repository and
// machine, the main tree's. A line with its entries and size, opening the
// list with a remove action per entry. The next `prepare` of an entry's
// inputs runs and makes it again.
import { computed, onMounted, ref, watch } from 'vue';
import { api, paths, v1, Unreachable } from '@/api';
import type { StoreEntry, StoreView } from '@/api/types';
import { act } from '@/lib/actions';
import { ago, bytes, dateTime, useNow } from '@/lib/format';
import { shortTime, storeLine, treeLabel } from '@/lib/store';
import { useStack } from '@/stores/stack';
import Modal from './Modal.vue';
import Icon from './icons/Icon.vue';

const stack = useStack();
const now = useNow();
const view = ref<StoreView | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);
const open = ref(false);
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
watch(open, (o) => {
  if (o) void load();
});
const entries = computed(() => view.value?.entries ?? []);
const canRemove = computed(() => !!view.value?.owned && !stack.readOnly);
</script>

<template>
  <section v-if="view" class="card store-line" data-testid="store-line">
    <div class="card-body line">
      <Icon name="hardDrive" :size="16" class="muted" />
      <span><strong>Store</strong> of what <code>prepare</code> makes: <span class="tabular" data-testid="store-summary">{{ storeLine(view) }}</span></span>
      <span class="grow" />
      <button type="button" class="btn btn-sm" :disabled="!entries.length" data-testid="store-open" @click="open = true"><Icon name="search" :size="14" /> Entries</button>
    </div>
  </section>
  <p v-else-if="error" class="faint small">Store: {{ error }}</p>

  <Modal :open="open" :title="`Store · ${storeLine(view ?? { entries: [], bytes: 0 })}`" size="xl" testid="store-modal" @close="open = false">
    <template #icon><Icon name="hardDrive" :size="18" /></template>
    <p class="muted small">
      What <code>prepare</code> made (its <code>outputs</code>), by its inputs, this machine's platform and its <code>version</code>: a lane or a build with the same
      inputs copies it instead of running <code>prepare</code>. An entry not used for 14 days goes by itself.
    </p>
    <div v-if="!entries.length" class="banner neutral"><Icon name="info" /> The store is empty.</div>
    <div v-else class="table-wrap">
      <table class="table" data-testid="store-table">
        <thead>
          <tr>
            <th>Service</th>
            <th>Key</th>
            <th>Outputs</th>
            <th class="num">Size</th>
            <th class="hide-sm">Made</th>
            <th>Last used</th>
            <th class="hide-sm">Used by</th>
            <th v-if="canRemove" />
          </tr>
        </thead>
        <tbody>
          <tr v-for="e in entries" :key="id(e)" :data-entry="id(e)">
            <td class="strong">{{ e.service }}</td>
            <td class="mono" :title="e.key">{{ e.key.slice(0, 8) }}</td>
            <td class="mono small">{{ e.outputs.join(', ') }}</td>
            <td class="num tabular" :title="`${e.files.toLocaleString()} files`">{{ bytes(e.bytes) }}</td>
            <td class="hide-sm small" :title="dateTime(e.created)">
              {{ ago(e.created, now) }} · prepare {{ shortTime(e.prepare_ms) }}<template v-if="e.copy_ms !== undefined">, copy {{ shortTime(e.copy_ms) }}</template>
            </td>
            <td class="small" :title="dateTime(e.last_used)">{{ ago(e.last_used, now) }}</td>
            <td class="hide-sm small">{{ e.used_by.length ? e.used_by.map(treeLabel).join(', ') : '—' }}</td>
            <td v-if="canRemove" class="num">
              <button type="button" class="btn btn-sm" :disabled="removing === id(e)" :data-testid="`store-remove-${id(e)}`" title="Remove this entry" @click="remove(e)">
                <span v-if="removing === id(e)" class="spinner" /><Icon v-else name="trash" :size="14" /> Remove
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p v-if="view && !view.owned" class="faint small">This is the store of the node the lane runs on, read only here: the node removes its entries.</p>
    <template #footer>
      <button type="button" class="btn" :disabled="loading" @click="load"><Icon name="restart" /> Reload</button>
      <button type="button" class="btn btn-primary" @click="open = false">Close</button>
    </template>
  </Modal>
</template>

<style scoped>
.line {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.strong {
  font-weight: 600;
}
.small {
  font-size: 12px;
}
@media (max-width: 520px) {
  .hide-sm {
    display: none;
  }
}
</style>
