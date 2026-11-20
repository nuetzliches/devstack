<script setup lang="ts">
import { computed } from 'vue';
import type { ServiceView } from '@/api/types';
import { linkLabel, serviceUrl, webUrl } from '@/lib/format';
import { statusBucket } from '@/lib/status';
import { useStack } from '@/stores/stack';
import Icon from '@/components/icons/Icon.vue';

const props = defineProps<{ service: ServiceView }>();
const stack = useStack();

/** A service with no port and no web link has nothing to open: the rule `serviceUrl` takes its link by. */
const url = computed(() => {
  const s = props.service;
  return webUrl(s.link) || webUrl(s.url) || s.port ? serviceUrl(s) : null;
});
const bucket = computed(() => statusBucket(props.service));
const tone: Record<ReturnType<typeof statusBucket>, string> = { healthy: 'ok', starting: 'accent', failed: 'danger', idle: 'neutral' };
</script>

<template>
  <article class="card shortcut-card" :class="`tone-${tone[bucket]}`" :data-testid="'shortcut-' + service.name" :data-state="service.status.state">
    <header class="shortcut-head">
      <span class="shortcut-icon" aria-hidden="true"><Icon name="globe" :size="18" /></span>
      <div class="shortcut-title">
        <span class="shortcut-name" :title="service.name">{{ service.name }}</span>
        <span class="shortcut-state">
          <span class="shortcut-dot" aria-hidden="true" />{{ service.status.state }}
        </span>
      </div>
    </header>
    <a
      v-if="url"
      class="shortcut-open"
      :href="url"
      target="_blank"
      rel="noopener"
      :title="`Open ${service.name} (${linkLabel(url)})`"
      data-testid="shortcut-open"
      @click="stack.followLink($event, url)"
      @auxclick="stack.followLink($event, url)"
    >
      <span class="shortcut-label">{{ service.name }}</span>
      <Icon name="external" :size="14" />
    </a>
    <span v-else class="muted">No port or link to open</span>
    <span v-if="url" class="shortcut-url muted">{{ linkLabel(url) }}</span>
  </article>
</template>

<style scoped>
.shortcut-card {
  --tone: var(--accent);
  --tone-soft: var(--accent-soft);
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 16px 18px 14px;
  /* As wide as its name needs, up to a cap; past it the name is cut off and the tooltip holds it whole. */
  flex: 0 1 auto;
  min-width: 180px;
  max-width: min(340px, 100%);
  overflow: hidden;
  border-color: color-mix(in srgb, var(--tone) 45%, var(--border));
  background: linear-gradient(160deg, var(--tone-soft), var(--surface) 65%);
  box-shadow: var(--shadow), 0 0 0 1px color-mix(in srgb, var(--tone) 12%, transparent);
  transition:
    transform 0.15s ease,
    box-shadow 0.15s ease;
}
.shortcut-card::before {
  content: '';
  position: absolute;
  inset: 0 0 auto 0;
  height: 3px;
  background: linear-gradient(90deg, var(--tone), transparent);
}
.shortcut-card:hover {
  transform: translateY(-2px);
  box-shadow: var(--shadow-lg);
}
.tone-ok {
  --tone: var(--ok);
  --tone-soft: var(--ok-soft);
}
.tone-accent {
  --tone: var(--accent);
  --tone-soft: var(--accent-soft);
}
.tone-danger {
  --tone: var(--danger);
  --tone-soft: var(--danger-soft);
}
.tone-neutral {
  --tone: var(--neutral-text);
  --tone-soft: var(--neutral-soft);
}
.shortcut-head {
  display: flex;
  align-items: center;
  gap: 12px;
}
.shortcut-icon {
  display: grid;
  place-items: center;
  width: 38px;
  height: 38px;
  flex: none;
  border-radius: var(--radius);
  color: var(--tone);
  background: color-mix(in srgb, var(--tone) 16%, transparent);
}
.shortcut-title {
  display: flex;
  flex-direction: column;
  min-width: 0;
  flex: 1;
}
.shortcut-name {
  font-weight: 700;
  font-size: 1.1rem;
  letter-spacing: 0.01em;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.shortcut-state {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 0.78rem;
  color: var(--tone);
}
.shortcut-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--tone);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--tone) 25%, transparent);
}
.shortcut-open {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 7px 12px;
  border-radius: var(--radius);
  background: var(--accent);
  color: var(--on-accent);
  font-size: 0.9rem;
  font-weight: 500;
  text-decoration: none;
  box-shadow: 0 3px 10px -6px color-mix(in srgb, var(--accent) 70%, transparent);
  transition:
    background 0.15s ease,
    transform 0.15s ease;
}
.shortcut-open:hover {
  background: var(--accent-hover);
  transform: scale(1.015);
}
.shortcut-label {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.shortcut-open :deep(svg) {
  flex: none;
}
.shortcut-open:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 3px;
}
.shortcut-url {
  font-size: 0.78rem;
  font-family: var(--font-mono, ui-monospace, monospace);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
