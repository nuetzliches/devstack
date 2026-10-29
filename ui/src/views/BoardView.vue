<script setup lang="ts">
// The developer's board (idea #378, a mock): one inbox for what needs them,
// then the tiles their agent built from a file the hub keeps, and the tiles
// their pools hand out. The dashboard renders; the agent edits.
import { computed, onMounted, onUnmounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { api, paths } from '@/api';
import type { AttentionItem, Board, BoardTile } from '@/api/board';
import { useUi } from '@/stores/ui';
import Icon from '@/components/icons/Icon.vue';
import type { IconName } from '@/components/icons/icons';
import BoardElement from '@/components/board/BoardElement.vue';

const ui = useUi();
const router = useRouter();
const board = ref<Board | null>(null);
const inbox = ref<AttentionItem[]>([]);
const showSeen = ref(false);

async function load() {
  const [b, a] = await Promise.all([api().request<Board>('GET', paths.board), api().request<{ items: AttentionItem[] }>('GET', paths.attention)]);
  if (b.ok && b.body && typeof b.body === 'object') board.value = b.body;
  if (a.ok && a.body && typeof a.body === 'object') inbox.value = a.body.items;
}

// The mock reloads every few seconds; the hub would tell /hub/events instead.
let timer: number | undefined;
onMounted(() => {
  void load();
  timer = window.setInterval(() => void load(), 5000);
});
onUnmounted(() => window.clearInterval(timer));

const open = computed(() => inbox.value.filter((i) => !i.seen));
const seen = computed(() => inbox.value.filter((i) => i.seen));

async function act(i: AttentionItem, what: 'seen' | 'snooze' | 'done') {
  await api().request('POST', paths.attentionAct(i.id, what));
  await load();
}

function follow(i: AttentionItem) {
  const a = i.action;
  if (!a) return;
  if (a.kind === 'link' && a.url) window.open(a.url, '_blank', 'noopener');
  else if (a.stack) void router.push({ name: 'overview', params: { stack: a.stack } });
  if (a.kind === 'promote') ui.toast('info', 'Promote', 'The mock opens the stack; the real inbox promotes here.');
}

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    ui.toast('success', `${what} copied`, 'Paste it into your coding agent.');
  } catch {
    ui.toast('error', 'Could not copy', text);
  }
}

function changeTile(t: BoardTile) {
  const file = board.value?.file ?? 'board.toml';
  if (t.origin.kind === 'pool')
    void copy(
      `Copy the tile "${t.title}" that the pool ${t.origin.pool} hands out into my devstack board (${file}) as a tile of my own, then change the copy: `,
      'Prompt',
    );
  else void copy(`Change the tile "${t.title}" (id ${t.id}) of my devstack board (${file}), with the tools board_get, board_try and board_set: `, 'Prompt');
}

function newTile() {
  void copy(
    `Add a tile to my devstack board (${board.value?.file ?? 'board.toml'}): <what you want to see>. Use the skill devstack-board; try the source with board_try before board_set. If the board cannot show it, draft a devstack issue instead and ask me before filing it.`,
    'Prompt',
  );
}

function ago(t: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000) - t);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  return `${Math.floor(s / 3600)} h ago`;
}

const originLabel: Record<AttentionItem['origin'], string> = { devstack: 'devstack', tile: 'tile', push: 'pushed', pool: 'pool' };
</script>

<template>
  <div class="page board">
    <header class="page-header">
      <div>
        <h1>Board</h1>
        <p class="muted small">
          What needs you, and what your agent put here. Your tiles come from <code>{{ board?.file ?? '…' }}</code>; your agent changes them, the
          dashboard only shows them.
        </p>
      </div>
      <button type="button" class="btn btn-sm" data-testid="board-new-tile" @click="newTile"><Icon name="plus" :size="14" /> New tile with your agent</button>
    </header>

    <section class="card inbox" :class="{ calm: !open.length }" data-testid="board-inbox">
      <header class="card-header">
        <Icon name="bell" :size="16" />
        <h2>Needs you</h2>
        <span v-if="open.length" class="pill sm warn">{{ open.length }}</span>
        <span class="spacer" />
        <button v-if="seen.length" type="button" class="btn btn-ghost btn-sm" @click="showSeen = !showSeen">{{ showSeen ? 'Hide' : 'Show' }} seen ({{ seen.length }})</button>
      </header>
      <p v-if="!open.length" class="muted calm-line"><Icon name="check" :size="14" /> Nothing waits for you.</p>
      <ul class="items">
        <li v-for="i in showSeen ? inbox : open" :key="i.id" class="item" :class="[i.state, { seen: i.seen }]" :data-testid="`inbox-${i.id}`">
          <span class="bar" />
          <div class="body">
            <div class="head">
              <span class="pill sm outline">{{ originLabel[i.origin] }}</span>
              <span class="muted small">{{ i.from }}</span>
              <span v-if="i.mandatory" class="pill sm info" title="Your pool sends this; it stays until you have seen it">from your pool</span>
              <span class="faint small">{{ ago(i.time) }}</span>
            </div>
            <strong>{{ i.title }}</strong>
            <span v-if="i.detail" class="muted small detail">{{ i.detail }}</span>
          </div>
          <div class="acts">
            <button v-if="i.action" type="button" class="btn btn-sm" :class="{ 'btn-primary': i.action.kind === 'promote' }" @click="follow(i)">
              {{ i.action.label }}
            </button>
            <button v-if="!i.seen" type="button" class="btn btn-ghost btn-sm" title="Seen: it leaves the list, the alarm stays where it came from" @click="act(i, 'seen')">
              Seen
            </button>
            <button v-if="!i.mandatory || i.seen" type="button" class="btn btn-ghost btn-sm" title="Back in an hour" @click="act(i, 'snooze')">
              <Icon name="clock" :size="13" />
            </button>
          </div>
        </li>
      </ul>
    </section>

    <div class="grid">
      <section v-for="t in board?.tiles ?? []" :key="t.id" class="card tile" :class="`w${t.width}`" :data-testid="`tile-${t.id}`">
        <header class="card-header">
          <Icon :name="(t.icon ?? 'grid') as IconName" :size="15" />
          <h2>{{ t.title }}</h2>
          <span v-if="t.origin.kind === 'pool'" class="pill sm" :class="t.origin.mandatory ? 'info' : 'outline'" :title="t.origin.mandatory ? 'Your pool sets it for everyone; you can collapse it, not remove it' : 'Your pool offered it and you accepted it'">
            {{ t.origin.pool }} · {{ t.origin.mandatory ? 'set by the pool' : 'accepted' }}
          </span>
          <span class="spacer" />
          <button type="button" class="btn btn-ghost btn-sm icon-only" :title="t.origin.kind === 'pool' ? 'Copy into your board with your agent' : 'Change with your agent'" @click="changeTile(t)">
            <Icon :name="t.origin.kind === 'pool' ? 'copy' : 'wrench'" :size="13" />
          </button>
        </header>
        <div class="tile-body">
          <BoardElement :el="t.element" />
        </div>
        <footer class="tile-foot faint small">
          <code :title="t.source">{{ t.source }}</code>
          <span class="tabular">{{ ago(t.refreshed) }}<template v-if="t.every"> · every {{ t.every }}</template></span>
        </footer>
      </section>
      <button type="button" class="card tile add" @click="newTile">
        <Icon name="plus" :size="20" />
        <span>A tile for anything your agent can reach</span>
        <span class="faint small">An MCP tool, a command, a URL. What the board cannot show becomes an issue, not a plugin.</span>
      </button>
    </div>
  </div>
</template>

<style scoped>
.board h1 {
  margin: 0 0 4px;
  font-size: 20px;
}
.board h2 {
  margin: 0;
  font-size: 14px;
}
.spacer {
  flex: 1;
}
.inbox.calm {
  opacity: 0.85;
}
.calm-line {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 0;
  padding: 4px 16px 14px;
}
.items {
  list-style: none;
  margin: 0;
  padding: 0 12px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.item {
  display: flex;
  align-items: stretch;
  gap: 12px;
  padding: 10px 12px 10px 0;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface);
}
.item.seen {
  opacity: 0.6;
}
.bar {
  width: 4px;
  border-radius: 8px 0 0 8px;
  background: var(--text-faint);
}
.item.err .bar {
  background: var(--danger);
}
.item.warn .bar {
  background: var(--warn);
}
.item.info .bar {
  background: var(--info);
}
.body {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.detail {
  font-family: var(--font-mono, monospace);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.acts {
  display: flex;
  align-items: center;
  gap: 4px;
}
.grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--space-4);
}
@media (max-width: 1100px) {
  .grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
@media (max-width: 720px) {
  .grid {
    grid-template-columns: minmax(0, 1fr);
  }
}
.tile {
  display: flex;
  flex-direction: column;
  margin: 0;
}
.tile.w2 {
  grid-column: span 2;
}
.tile.w3 {
  grid-column: 1 / -1;
}
@media (max-width: 720px) {
  .tile.w2 {
    grid-column: auto;
  }
}
.tile-body {
  flex: 1;
  padding: 4px 16px 12px;
}
.tile-foot {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 16px;
  border-top: 1px solid var(--border);
}
.tile-foot code {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
}
.tile.add {
  align-items: center;
  justify-content: center;
  gap: 6px;
  min-height: 160px;
  border: 1.5px dashed var(--border-strong);
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
  text-align: center;
  padding: 16px;
}
.tile.add:hover {
  color: var(--text);
  border-color: var(--accent);
}
</style>
