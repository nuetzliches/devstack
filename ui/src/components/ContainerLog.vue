<script setup lang="ts">
// A container's output (`docker compose logs`, stdout and stderr in one,
// timestamped), inline on its page. Refreshed every few seconds unless paused.
import { onBeforeUnmount, ref, watch } from 'vue';
import { api, paths, v1, Unreachable } from '@/api';
import Diagnostics from './Diagnostics.vue';
import Icon from './icons/Icon.vue';

const props = defineProps<{ stackId: string; group: string; service: string }>();

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
  () => [props.stackId, props.group, props.service] as const,
  () => {
    text.value = null;
    void load();
  },
  { immediate: true },
);
watch(
  follow,
  (f) => {
    stop();
    if (f) timer = setInterval(() => void load(), 3000);
  },
  { immediate: true },
);
onBeforeUnmount(stop);
</script>

<template>
  <section class="card" data-testid="container-log">
    <header class="card-header">
      <h2>Log</h2>
      <span class="grow" />
      <label class="check"><input v-model="follow" type="checkbox" data-testid="container-log-follow" /> Refresh every 3 s</label>
      <button type="button" class="btn btn-sm" :disabled="loading" @click="load"><Icon name="restart" :size="14" /> Reload</button>
    </header>
    <div class="body">
      <div v-if="text === null && loading" class="row muted"><span class="spinner" /> Loading…</div>
      <div v-else-if="error && text === null" class="banner neutral"><Icon name="info" /> {{ error }}</div>
      <Diagnostics v-else-if="text !== null" :text="text || '(no output)'" max-height="70vh" />
      <p class="muted hint">The last 1000 lines of <code>docker compose logs</code>, stdout and stderr together, with Docker's timestamps.</p>
    </div>
  </section>
</template>

<style scoped>
.body {
  padding: 12px 16px 16px;
}
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
