<script setup lang="ts">
// What changed in the releases between the version this browser last saw and
// the one installed: each release's summary, its bullets one click away
// (stores/whatsNew.ts). The text is CHANGELOG.md.
import { useWhatsNew } from '@/stores/whatsNew';
import ChangelogText from './ChangelogText.vue';
import Modal from './Modal.vue';

const news = useWhatsNew();
const CHANGELOG_URL = 'https://github.com/nuetzliches/devstack/blob/main/CHANGELOG.md';
</script>

<template>
  <Modal :open="news.open" title="What's new in devstack" size="lg" testid="whats-new" @close="news.dismiss()">
    <p v-if="!news.shown.length" class="faint">Nothing is listed for devstack {{ news.current }}.</p>
    <section v-for="s in news.shown" :key="s.version" class="release" :data-testid="`whats-new-${s.version}`">
      <h3>
        {{ s.version }}
        <span v-if="s.date" class="faint date">{{ s.date }}</span>
      </h3>
      <p v-for="(p, i) in s.summary" :key="i"><ChangelogText :text="p" /></p>
      <details v-if="s.groups.length" :open="!s.summary.length">
        <summary>Details</summary>
        <template v-for="g in s.groups" :key="g.heading">
          <h4>{{ g.heading }}</h4>
          <ul>
            <li v-for="(item, i) in g.items" :key="i"><ChangelogText :text="item" /></li>
          </ul>
        </template>
      </details>
    </section>
    <p v-if="news.omitted > 0" class="faint" data-testid="whats-new-omitted">
      {{ news.omitted }} older release{{ news.omitted === 1 ? '' : 's' }} not listed: see the full changelog.
    </p>
    <template #footer>
      <a class="btn btn-sm" :href="CHANGELOG_URL" target="_blank" rel="noopener noreferrer">Full changelog</a>
      <button type="button" class="btn btn-sm btn-primary" data-testid="whats-new-close" @click="news.dismiss()">Close</button>
    </template>
  </Modal>
</template>

<style scoped>
.release + .release {
  margin-top: var(--space-4);
  padding-top: var(--space-3);
  border-top: 1px solid var(--border);
}
h3 {
  font-size: 15px;
  margin: 0 0 6px;
}
h4 {
  font-size: 13px;
  margin: 12px 0 4px;
  color: var(--text-2);
}
.date {
  font-size: 12px;
  font-weight: 400;
  margin-left: 6px;
}
p {
  margin: 0 0 8px;
  line-height: 1.5;
}
summary {
  cursor: pointer;
  color: var(--text-2);
  font-size: 13px;
}
ul {
  margin: 0;
  padding-left: 20px;
}
li {
  margin: 4px 0;
  line-height: 1.45;
  font-size: 13px;
}
.faint {
  color: var(--text-muted);
}
</style>
