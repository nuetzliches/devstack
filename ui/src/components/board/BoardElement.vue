<script setup lang="ts">
// Renders one element of the board's display protocol (idea #378): data
// only, in a fixed vocabulary; what it cannot show is a wish for a new element.
import type { BoardElement, Tone } from '@/api/board';
import Sparkline from '@/components/Sparkline.vue';

defineProps<{ el: BoardElement }>();

function ago(t: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000) - t);
  if (s < 60) return 'now';
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86400) return `${Math.floor(s / 3600)} h`;
  return `${Math.floor(s / 86400)} d`;
}

const cellTone = (v: string | number): Tone | null => (v === 'green' ? 'ok' : v === 'red' ? 'err' : v === 'running' ? 'info' : null);
</script>

<template>
  <ul v-if="el.type === 'list'" class="el-list">
    <li v-for="(i, n) in el.items" :key="n">
      <span class="dot" :class="i.state ?? 'idle'" />
      <span v-if="i.badge" class="badge tabular">{{ i.badge }}</span>
      <a v-if="i.url" :href="i.url" target="_blank" rel="noopener" class="title">{{ i.title }}</a>
      <span v-else class="title">{{ i.title }}</span>
      <span v-if="i.detail" class="muted small detail">{{ i.detail }}</span>
      <span v-if="i.time" class="faint small tabular">{{ ago(i.time) }}</span>
    </li>
    <li v-if="!el.items.length" class="muted">{{ el.empty ?? 'Nothing.' }}</li>
  </ul>

  <ul v-else-if="el.type === 'status'" class="el-status">
    <li v-for="(i, n) in el.items" :key="n" :class="i.state">
      <span class="dot" :class="i.state" />
      <span class="title">{{ i.label }}</span>
      <span v-if="i.detail" class="small tabular detail">{{ i.detail }}</span>
    </li>
  </ul>

  <div v-else-if="el.type === 'number'" class="el-number">
    <div class="big tabular" :class="el.state">
      {{ el.value }}<span v-if="el.unit" class="unit">{{ el.unit }}</span>
      <span v-if="el.delta !== undefined" class="delta small" :class="el.delta > 0 ? 'up' : 'down'">{{ el.delta > 0 ? '+' : '' }}{{ el.delta }}</span>
    </div>
    <div v-if="el.label" class="muted small">{{ el.label }}</div>
    <Sparkline v-if="el.trend" :values="el.trend" fluid :height="36" :color="el.state === 'warn' ? 'var(--warn)' : 'var(--accent)'" />
  </div>

  <div v-else-if="el.type === 'text'" class="el-text">{{ el.text }}</div>

  <ul v-else-if="el.type === 'links'" class="el-links">
    <li v-for="(i, n) in el.items" :key="n">
      <a :href="i.url" target="_blank" rel="noopener">{{ i.label }}</a>
      <span v-if="i.detail" class="muted small">{{ i.detail }}</span>
    </li>
  </ul>

  <table v-else-if="el.type === 'table'" class="el-table">
    <thead>
      <tr>
        <th v-for="c in el.columns" :key="c">{{ c }}</th>
      </tr>
    </thead>
    <tbody>
      <tr v-for="(r, n) in el.rows" :key="n">
        <td v-for="(v, k) in r" :key="k">
          <span v-if="cellTone(v)" class="pill sm" :class="cellTone(v) === 'err' ? 'danger' : cellTone(v)">{{ v }}</span>
          <template v-else>{{ v }}</template>
        </td>
      </tr>
    </tbody>
  </table>

  <ol v-else-if="el.type === 'timeline'" class="el-timeline">
    <li v-for="(i, n) in el.items" :key="n">
      <span class="dot" :class="i.state ?? 'idle'" />
      <span class="faint small tabular when">{{ ago(i.time) }}</span>
      <span class="title">{{ i.title }}</span>
      <span v-if="i.detail" class="muted small">{{ i.detail }}</span>
    </li>
  </ol>
</template>

<style scoped>
ul,
ol {
  list-style: none;
  margin: 0;
  padding: 0;
}
.dot {
  flex: none;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--text-faint);
}
.dot.ok {
  background: var(--ok);
}
.dot.warn {
  background: var(--warn);
}
.dot.err {
  background: var(--danger);
}
.dot.info {
  background: var(--accent);
}
.title {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.el-list li,
.el-status li,
.el-links li,
.el-timeline li {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 0;
  border-bottom: 1px solid var(--border);
}
.el-list li:last-child,
.el-status li:last-child,
.el-links li:last-child,
.el-timeline li:last-child {
  border-bottom: 0;
}
.el-list .title,
.el-status .title {
  flex: 1;
}
.badge {
  font-size: 11px;
  color: var(--text-muted);
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0 5px;
}
.detail {
  flex: none;
}
.el-status li.err .detail {
  color: var(--danger);
}
.el-status li.warn .detail {
  color: var(--warn);
}
.el-number .big {
  font-size: 34px;
  font-weight: 600;
  line-height: 1.1;
  display: flex;
  align-items: baseline;
  gap: 8px;
}
.el-number .big.warn {
  color: var(--warn);
}
.el-number .big.err {
  color: var(--danger);
}
.unit {
  font-size: 16px;
}
.delta.up {
  color: var(--danger);
}
.delta.down {
  color: var(--ok);
}
.el-text {
  white-space: pre-line;
  line-height: 1.6;
}
.el-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}
.el-table th {
  text-align: left;
  font-weight: 500;
  color: var(--text-muted);
  padding: 4px 8px 6px 0;
  border-bottom: 1px solid var(--border);
}
.el-table td {
  padding: 6px 8px 6px 0;
  border-bottom: 1px solid var(--border);
}
.el-table tr:last-child td {
  border-bottom: 0;
}
.when {
  width: 44px;
  flex: none;
}
</style>
