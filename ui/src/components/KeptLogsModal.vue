<script setup lang="ts">
// The logs devstack keeps per service (plan §11): one per started instance,
// one per build. What an instance printed before it crashed or was replaced.
import { computed, ref, watch } from 'vue';
import { api, paths, v1, Unreachable } from '@/api';
import { dateTime, bytes, shortBuild } from '@/lib/format';
import Modal from './Modal.vue';
import Diagnostics from './Diagnostics.vue';
import Icon from './icons/Icon.vue';

const props = defineProps<{ open: boolean; stackId: string; lane?: string | null; service: string; initialFile?: string }>();
const emit = defineEmits<{ close: [] }>();

type KeptLog = { file: string; kind: 'run' | 'build'; started_ms: number; modified_ms: number; build: string | null; port: number | null; bytes: number; current: boolean };

const files = ref<KeptLog[]>([]);
const chosen = ref<string | null>(null);
const text = ref<string | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);
const kind = ref<'all' | 'run' | 'build'>('all');
const shown = computed(() => files.value.filter((f) => kind.value === 'all' || f.kind === kind.value));

function failed(e: unknown, status?: number) {
  error.value = e instanceof Unreachable ? 'The hub is not reachable.' : status === 503 ? 'The stack is not running.' : status ? `HTTP ${status}` : String(e);
}

async function load() {
  loading.value = true;
  error.value = null;
  try {
    const res = await api().request<KeptLog[]>('GET', paths.api(props.stackId, v1.logFiles(props.service), props.lane ?? null));
    if (res.ok && Array.isArray(res.body)) {
      files.value = res.body;
      const first = res.body.find((f) => f.file === props.initialFile) ?? res.body.find((f) => !f.current && f.kind === 'run') ?? res.body[0];
      if (first && !chosen.value) await show(first.file);
    } else failed(null, res.status);
  } catch (e) {
    failed(e);
  } finally {
    loading.value = false;
  }
}

async function show(file: string) {
  chosen.value = file;
  text.value = null;
  try {
    const res = await api().request<string>('GET', paths.api(props.stackId, v1.keptLog(props.service, file), props.lane ?? null));
    if (res.ok) text.value = typeof res.body === 'string' ? res.body : '';
    else failed(null, res.status);
  } catch (e) {
    failed(e);
  }
}

watch(
  () => props.open,
  (o) => {
    if (o) {
      chosen.value = null;
      text.value = null;
      void load();
    }
  },
  { immediate: true },
);
</script>

<template>
  <Modal :open="open" :title="`Earlier logs · ${service}`" size="xl" testid="kept-logs-modal" @close="emit('close')">
    <template #icon><Icon name="history" :size="18" /></template>
    <div v-if="loading && !files.length" class="row muted"><span class="spinner" /> Loading…</div>
    <div v-else-if="error" class="banner neutral"><Icon name="info" /> {{ error }}</div>
    <div v-else-if="!files.length" class="banner neutral"><Icon name="info" /> devstack has kept no log of this service yet.</div>
    <div v-else class="split">
      <div class="list">
        <div class="seg" role="group" aria-label="Kind">
          <button v-for="k in ['all', 'run', 'build'] as const" :key="k" type="button" class="btn btn-sm" :class="{ active: kind === k }" @click="kind = k">
            {{ k === 'all' ? 'All' : k === 'run' ? 'Instances' : 'Builds' }}
          </button>
        </div>
        <button v-for="f in shown" :key="f.file" type="button" class="item" :class="{ active: chosen === f.file }" :data-testid="`kept-log-${f.file}`" @click="show(f.file)">
          <span class="when">
            <Icon :name="f.kind === 'build' ? 'file' : 'terminal'" :size="13" />
            {{ dateTime(f.kind === 'build' ? f.modified_ms : f.started_ms) }}
            <span v-if="f.current" class="tag">running</span>
          </span>
          <span class="meta muted">
            <template v-if="f.build">build {{ shortBuild(f.build) }}</template><template v-if="f.port"> · port {{ f.port }}</template> · {{ bytes(f.bytes) }}
          </span>
        </button>
      </div>
      <div class="content">
        <div v-if="chosen && text === null" class="row muted"><span class="spinner" /> Loading…</div>
        <Diagnostics v-else-if="text !== null" :text="text || '(empty)'" max-height="62vh" />
        <p class="muted hint">The last 2000 lines. Lines carry no time of their own; the list says when an instance started.</p>
      </div>
    </div>
    <template #footer>
      <button type="button" class="btn" :disabled="loading" @click="load"><Icon name="restart" /> Reload</button>
      <button type="button" class="btn btn-primary" @click="emit('close')">Close</button>
    </template>
  </Modal>
</template>

<style scoped>
.split {
  display: grid;
  grid-template-columns: minmax(200px, 260px) 1fr;
  gap: 12px;
  min-height: 0;
}
@media (max-width: 700px) {
  .split {
    grid-template-columns: 1fr;
  }
}
.list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-height: 66vh;
  overflow: auto;
}
.seg {
  display: flex;
  gap: 4px;
  margin-bottom: 6px;
}
.seg .active {
  background: var(--accent-soft, rgba(0, 0, 0, 0.08));
}
.item {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  text-align: left;
  padding: 6px 8px;
  border: 1px solid transparent;
  border-radius: 8px;
  background: none;
  color: inherit;
  cursor: pointer;
  font: inherit;
}
.item:hover {
  border-color: var(--border);
}
.item.active {
  border-color: var(--accent);
}
.when {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
}
.meta {
  font-size: 12px;
}
.tag {
  font-size: 11px;
  padding: 0 6px;
  border-radius: 6px;
  border: 1px solid var(--accent);
}
.content {
  min-width: 0;
}
.hint {
  font-size: 12px;
  margin: 6px 0 0;
}
</style>
