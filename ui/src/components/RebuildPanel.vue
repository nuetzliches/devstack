<script setup lang="ts">
// A dev server's last rebuild (ADR-0042), where a built service shows its
// candidate: under way, done in how long, or why it failed, with the dev
// server's own error lines.
import { computed } from 'vue';
import type { Rebuild } from '@/api/types';
import { ago, useNow } from '@/lib/format';
import { rebuildLook, rebuildSummary } from '@/lib/status';
import Icon from './icons/Icon.vue';

const props = defineProps<{ rebuild: Rebuild }>();
const emit = defineEmits<{ log: [] }>();
const now = useNow();
const look = computed(() => rebuildLook(props.rebuild));
</script>

<template>
  <section class="reb" :class="`tone-${look.tone}`" data-testid="rebuild-banner" :data-state="rebuild.state" aria-live="polite">
    <div class="reb-head">
      <div class="reb-icon" aria-hidden="true">
        <span v-if="look.busy" class="spinner" />
        <Icon v-else :name="look.icon" :size="18" />
      </div>
      <div class="grow">
        <div class="reb-title">
          <span class="eyebrow">Dev server</span>
          <span class="state">{{ look.label }}</span>
          <span class="tag">{{ rebuild.tool }}</span>
          <span class="muted since">· {{ rebuildSummary(rebuild) }} · {{ ago(rebuild.since, now) }}</span>
        </div>
        <p class="explain">{{ look.explain }}</p>
      </div>
      <button type="button" class="btn btn-sm" data-testid="rebuild-log" @click="emit('log')"><Icon name="file" :size="14" /> Log</button>
    </div>
    <pre v-if="rebuild.errors?.length" class="errors" data-testid="rebuild-errors">{{ rebuild.errors.join('\n') }}</pre>
  </section>
</template>

<style scoped>
.reb {
  --tone: var(--accent);
  border: 1px solid color-mix(in srgb, var(--tone) 35%, transparent);
  background: color-mix(in srgb, var(--tone) 8%, var(--surface));
  border-radius: var(--radius);
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.reb.tone-ok {
  --tone: var(--ok);
}
.reb.tone-danger {
  --tone: var(--danger);
}
.reb.tone-neutral {
  --tone: var(--text-faint);
}
.reb-head {
  display: flex;
  align-items: flex-start;
  gap: 12px;
}
.reb-icon {
  width: 34px;
  height: 34px;
  border-radius: 999px;
  display: grid;
  place-items: center;
  color: var(--tone);
  background: color-mix(in srgb, var(--tone) 14%, transparent);
  flex: none;
}
.reb-title {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: 8px;
}
.eyebrow {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--tone);
}
.state {
  font-weight: 700;
  font-size: 15px;
}
.tag {
  font-family: var(--font-mono);
  font-size: 11.5px;
  padding: 1px 6px;
  border-radius: 6px;
  background: var(--surface-2);
}
.explain {
  margin: 4px 0 0;
  color: var(--text-muted);
  font-size: 13px;
}
.errors {
  margin: 0;
  padding: 10px 12px;
  border-radius: 8px;
  background: var(--bg);
  border: 1px solid var(--border);
  font-family: var(--font-mono);
  font-size: 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  max-height: 240px;
  overflow: auto;
}
</style>
