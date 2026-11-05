<script setup lang="ts">
// A newer devstack runs than this browser last saw: said on every page, below
// the top bar, until it is opened or hidden (stores/whatsNew.ts).
import { useWhatsNew } from '@/stores/whatsNew';
import Icon from './icons/Icon.vue';

const news = useWhatsNew();
</script>

<template>
  <div v-if="news.banner" class="news-banner" role="status" data-testid="whats-new-banner">
    <div class="inner">
      <Icon name="info" :size="16" />
      <div class="grow">
        <strong>devstack {{ news.current }} is installed.</strong>
        <span class="faint"> It was {{ news.seen }} when you last looked.</span>
      </div>
      <button type="button" class="btn btn-sm" data-testid="whats-new-show" @click="news.show()">What's new</button>
      <button type="button" class="btn btn-ghost btn-sm btn-icon" title="Hide" aria-label="Hide" data-testid="whats-new-dismiss" @click="news.dismiss()">
        <Icon name="x" :size="14" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.news-banner {
  background: var(--info-soft);
  border-bottom: 1px solid color-mix(in srgb, var(--info) 30%, transparent);
  color: var(--info-text);
}
.inner {
  max-width: 1280px;
  margin: 0 auto;
  padding: 8px var(--space-5);
  display: flex;
  align-items: center;
  gap: 12px;
  font-size: 13px;
}
@media (max-width: 640px) {
  .inner {
    padding: 8px var(--space-3);
  }
}
.faint {
  opacity: 0.8;
}
</style>
