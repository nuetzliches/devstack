<script setup lang="ts">
import { computed, ref } from 'vue';
import { useRouter } from 'vue-router';
import type { CommandView } from '@/api/types';
import { useStack } from '@/stores/stack';
import Icon from './icons/Icon.vue';

const props = defineProps<{ command: CommandView }>();
const stack = useStack();
const router = useRouter();
const running = ref(false);
const lastRun = computed(() => stack.runs[props.command.name]?.[0]);

async function run() {
  running.value = true;
  try {
    const id = await stack.runCommand(props.command.name);
    if (id !== null) void router.push({ name: 'commands', params: { stack: stack.id ?? '' }, query: { cmd: props.command.name, run: String(id) } });
  } finally {
    running.value = false;
  }
}
</script>

<template>
  <li class="cmd-row" data-testid="command-row" :data-command="command.name">
    <span class="kind" title="command"><Icon name="terminal" :size="15" /></span>
    <div class="grow name-col">
      <div class="name">
        {{ command.name }}
        <Icon v-if="command.confirm" name="shield" :size="13" class="confirm-mark" label="asks for confirmation" />
      </div>
      <div class="desc truncate" :title="command.description">{{ command.description }}</div>
    </div>
    <span v-if="lastRun?.status === 'running'" class="pill accent pulse sm"><span class="dot" />running</span>
    <button
      type="button"
      class="btn btn-sm"
      :disabled="running || stack.stale"
      :aria-label="`Run ${command.name}`"
      data-testid="command-run"
      @click="run"
    >
      <span v-if="running" class="spinner" /><Icon v-else name="play" :size="13" />
      Run
    </button>
  </li>
</template>

<style scoped>
.cmd-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 16px;
  border-bottom: 1px solid var(--border);
  min-width: 0;
}
.cmd-row:last-child {
  border-bottom: 0;
}
.kind {
  width: 28px;
  height: 28px;
  border-radius: 8px;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--warn) 12%, transparent);
  color: var(--warn-text);
  flex: none;
}
.name-col {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.name {
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 6px;
}
.confirm-mark {
  color: var(--warn-text);
}
.desc {
  font-size: 12px;
  color: var(--text-muted);
}
</style>
