<script setup lang="ts">
import { ref, watch } from 'vue';
import { api, paths, v1, Unreachable } from '@/api';
import Modal from './Modal.vue';
import Diagnostics from './Diagnostics.vue';
import Icon from './icons/Icon.vue';

const props = defineProps<{ open: boolean; stackId: string; lane?: string | null; service: string }>();
const emit = defineEmits<{ close: [] }>();

const text = ref<string | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);

async function load() {
  loading.value = true;
  error.value = null;
  text.value = null;
  try {
    const res = await api().request<string>('GET', paths.api(props.stackId, v1.buildLog(props.service), props.lane ?? null));
    if (res.ok) text.value = typeof res.body === 'string' ? res.body : res.body ? JSON.stringify(res.body, null, 2) : '';
    else error.value = res.status === 503 ? 'The stack is not running.' : res.status === 404 ? 'There is no build log for this service yet.' : `HTTP ${res.status}`;
  } catch (e) {
    error.value = e instanceof Unreachable ? 'The hub is not reachable.' : String(e);
  } finally {
    loading.value = false;
  }
}

watch(
  () => props.open,
  (o) => {
    if (o) void load();
  },
  { immediate: true },
);
</script>

<template>
  <Modal :open="open" :title="`Build log · ${service}`" size="xl" testid="build-log-modal" @close="emit('close')">
    <template #icon><Icon name="file" :size="18" /></template>
    <div v-if="loading" class="row muted"><span class="spinner" /> Loading…</div>
    <div v-else-if="error" class="banner neutral"><Icon name="info" /> {{ error }}</div>
    <Diagnostics v-else-if="text !== null" :text="text || '(empty)'" max-height="66vh" />
    <template #footer>
      <button type="button" class="btn" :disabled="loading" @click="load"><Icon name="restart" /> Reload</button>
      <button type="button" class="btn btn-primary" @click="emit('close')">Close</button>
    </template>
  </Modal>
</template>
