<script setup lang="ts">
import type { Tone } from '@/lib/status';

withDefaults(defineProps<{ tone: Tone; label: string; busy?: boolean; size?: 'sm' | 'md'; title?: string }>(), {
  busy: false,
  size: 'md',
  title: undefined,
});
</script>

<template>
  <span class="pill" :class="[tone, { pulse: busy, sm: size === 'sm' }]" :title="title">
    <span class="dot" aria-hidden="true" />
    {{ label }}
  </span>
</template>
