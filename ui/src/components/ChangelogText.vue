<script setup lang="ts">
// A line of the changelog: **bold** and `code` kept, nothing else made of it
// (no HTML from the file).
import { inline } from '@/lib/changelog';

defineProps<{ text: string }>();
</script>

<template>
  <span v-for="(p, i) in inline(text)" :key="i" :class="{ strong: p.strong }"><code v-if="p.code">{{ p.text }}</code><template v-else>{{ p.text }}</template></span>
</template>

<style scoped>
.strong {
  font-weight: 600;
}
</style>
