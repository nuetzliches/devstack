<script setup lang="ts">
// A container's output (`docker compose logs`, stdout and stderr in one,
// timestamped). Refreshed every few seconds while open, if wanted.
import { onBeforeUnmount, ref, watch } from 'vue';
import { api, paths, v1, Unreachable } from '@/api';
import Modal from './Modal.vue';
import Diagnostics from './Diagnostics.vue';
import Icon from './icons/Icon.vue';

const props = defineProps<{ open: boolean; stackId: string; group: string; service: string }>();
const emit = defineEmits<{ close: [] }>();

const text = ref<string | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);
const follow = ref(true);
let timer: ReturnType<typeof setInterval> | undefined;

async function load() {
  loading.value = true;
  try {
    const res = await api().request<string>('GET', paths.api(props.stackId, v1.infraLogs(props.group, props.service, 1000)));
    if (res.ok) {
      text.value = typeof res.body === 'string' ? res.body : '';
      error.value = null;
    } else {
      const b = res.body as { error?: string } | string | null;
      error.value = res.status === 503 ? 'The stack is not running.' : typeof b === 'object' && b?.error ? b.error : `HTTP ${res.status}`;
    }
  } catch (e) {
    error.value = e instanceof Unreachable ? 'The hub is not reachable.' : String(e);
  } finally {
    loading.value = false;
  }
}

function stop() {
  if (timer) clearInterval(timer);
  timer = undefined;
}

watch(
  () => [props.open, follow.value] as const,
  ([open, f]) => {
    stop();
    if (!open) return;
    if (text.value === null) void load();
    if (f) timer = setInterval(() => void load(), 3000);
  },
  { immediate: true },
);
watch(
  () => props.open,
  (o) => {
    if (o) {
      text.value = null;
      void load();
    }
  },
);
onBeforeUnmount(stop);
</script>

<template>
  <Modal :open="open" :title="`Container log · ${service}`" size="xl" testid="container-log-modal" @close="emit('close')">
    <template #icon><Icon name="box" :size="18" /></template>
    <div v-if="text === null && loading" class="row muted"><span class="spinner" /> Loading…</div>
    <div v-else-if="error && text === null" class="banner neutral"><Icon name="info" /> {{ error }}</div>
    <Diagnostics v-else-if="text !== null" :text="text || '(no output)'" max-height="66vh" />
    <p class="muted hint">The last 1000 lines of <code>docker compose logs</code>, stdout and stderr together, with Docker's timestamps.</p>
    <template #footer>
      <label class="check"><input v-model="follow" type="checkbox" /> Refresh every 3 s</label>
      <span class="grow" />
      <button type="button" class="btn" :disabled="loading" @click="load"><Icon name="restart" /> Reload</button>
      <button type="button" class="btn btn-primary" @click="emit('close')">Close</button>
    </template>
  </Modal>
</template>

<style scoped>
.hint {
  font-size: 12px;
  margin: 6px 0 0;
}
.check {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
}
.grow {
  flex: 1;
}
</style>
