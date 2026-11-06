<script setup lang="ts">
// The Skills page's flow (#482): when the hooks of the skills switched on give
// their text to the agents, and beside it what devstack did for the agent
// (each hook it gave, each tool call through `devstack mcp`). A click on a
// hook narrows the activity to it. The activity holds no prompt, text,
// argument or answer: only what happened.
import { computed, ref } from 'vue';
import type { ActivityRecord, ActivityView, PoolSkillsState } from '@/api/types';
import { conditionText, firings, refKey, flowNodes, onceText, phases, recordsFor, recordText, sessionLabel, toolsBySkill, type FlowNode } from '@/lib/flow';
import { ago } from '@/lib/format';

const props = defineProps<{
  state: PoolSkillsState | null;
  activity: ActivityView | null;
  /** The session shown (`''`: every one). */
  session: string;
  now: number;
}>();
const emit = defineEmits<{ (e: 'session', id: string): void }>();

const nodes = computed(() => flowNodes(props.state));
const flow = computed(() => phases(nodes.value));
const records = computed<ActivityRecord[]>(() => props.activity?.records ?? []);
const selected = ref<string | null>(null);
const picked = computed<FlowNode | null>(() => nodes.value.find((n) => n.key === selected.value) ?? null);
const shown = computed(() => recordsFor(records.value, picked.value).slice(0, 200));
const tools = computed(() => toolsBySkill(records.value));
const skillOf = (key: string) => nodes.value.find((n) => n.key === key)?.skill;
const colorOf = (skill: string | null | undefined) => nodes.value.find((n) => n.skill === skill)?.color;
/** Skills with hooks that are off or whose set is not accepted: not in the flow. */
const left = computed(() => (props.state?.sets ?? []).flatMap((s) => s.skills.filter((k) => k.hooks && (s.state !== 'accepted' || k.enabled === false)).map((k) => k.name)));
/** Skills with hooks whose hub sends no details (an older hub). */
const noDetails = computed(() =>
  (props.state?.sets ?? []).some((s) => s.state === 'accepted' && s.skills.some((k) => k.hooks && k.enabled !== false && !k.hook_details)),
);

function last(n: FlowNode): string {
  const f = firings(records.value, n.key)[0];
  return f ? `given ${ago(f.at, props.now)}` : 'not given yet';
}
/** A hook record in its skill's colour; a tool call square. */
function dotClass(r: ActivityRecord): string {
  if (r.kind === 'tool') return 'tool';
  const c = r.hooks[0] ? colorOf(skillOf(refKey(r.hooks[0]))) : undefined;
  return c === undefined ? '' : `c${c}`;
}
function pick(n: FlowNode) {
  selected.value = selected.value === n.key ? null : n.key;
}
const eventWords: Record<FlowNode['hook']['event'], string> = {
  'session-start': 'at the start of a session',
  'prompt-submit': 'when you send a prompt that matches',
  'lane-stop': 'when a lane stops (in the output of the stop)',
  'session-end': 'at the end of a session, given at the next start',
};
</script>

<template>
  <div class="flow-page" data-testid="skills-flow">
    <p v-if="!nodes.length" class="muted small" data-testid="skills-flow-empty">
      {{ noDetails ? 'This hub does not describe its hooks yet: update devstack.' : 'No skill switched on carries hooks.' }}
    </p>
    <div v-else class="flow-grid">
      <section class="card">
        <header class="card-header">
          <h2>What happens for your agent</h2>
          <span class="faint small">{{ nodes.length }} hook{{ nodes.length === 1 ? '' : 's' }} of {{ new Set(nodes.map((n) => n.skill)).size }} skill(s)</span>
        </header>
        <div class="card-body">
          <div class="phases" :style="{ gridTemplateColumns: `repeat(${flow.length}, minmax(0, 1fr))` }">
            <div v-for="(p, i) in flow" :key="p.id" class="phase" data-testid="skills-flow-phase" :data-phase="p.id">
              <h3><span class="num">{{ i + 1 }}</span> {{ p.title }}</h3>
              <p class="trigger faint small">{{ p.trigger }}</p>
              <div v-for="g in p.groups" :key="g.pattern ?? '-'" class="group">
                <code v-if="g.pattern" class="pattern faint" :title="g.pattern">matches {{ g.pattern }}</code>
                <button
                  v-for="n in g.nodes"
                  :key="n.key"
                  type="button"
                  class="node"
                  :class="[`c${n.color}`, { off: !n.hook.applies, picked: selected === n.key }]"
                  :aria-pressed="selected === n.key"
                  :aria-describedby="`tip-${n.key}`"
                  data-testid="skills-flow-node"
                  :data-key="n.key"
                  @click="pick(n)"
                >
                  <strong>{{ n.skill }}</strong>
                  <span class="faint small">{{ n.hook.upstream ? `needs ${n.hook.upstream}` : 'always' }}</span>
                  <span class="fired small" data-testid="skills-flow-fired">{{ n.hook.applies ? last(n) : 'not given: see why' }}</span>
                  <span :id="`tip-${n.key}`" role="tooltip" class="tip" data-testid="skills-flow-tip">
                    <span class="tip-title">{{ n.skill }} · hook {{ n.hook.index + 1 }}</span>
                    <span class="tip-row"><span class="k">When</span><span>{{ eventWords[n.hook.event] }}</span></span>
                    <span class="tip-row"><span class="k">Condition</span><span>{{ conditionText(n.hook) }}</span></span>
                    <span class="tip-row"><span class="k">How often</span><span>{{ onceText(n.hook) }}</span></span>
                    <span class="tip-row"><span class="k">Agents</span><span>{{ n.hook.event === 'lane-stop' ? 'every agent that reads the stop\'s output' : n.hook.agents.join(', ') || 'none switched on' }}</span></span>
                    <span v-if="!n.hook.applies" class="tip-row warn-text"><span class="k">Not given</span><span>{{ n.hook.why }}</span></span>
                    <span class="tip-text">“{{ n.hook.preview }}”</span>
                  </span>
                </button>
              </div>
            </div>
          </div>
          <p v-if="left.length" class="faint small left">Off, or in a set not accepted, and so not in the flow: {{ left.join(', ') }}.</p>

          <div v-if="tools.length" class="tools" data-testid="skills-flow-tools">
            <h3>Tools through devstack</h3>
            <div v-for="g in tools" :key="g.skill ?? '-'" class="tool-group">
              <span class="tool-skill" :class="colorOf(g.skill) !== undefined ? `c${colorOf(g.skill)}` : ''">{{ g.skill ?? 'tied to no skill' }}</span>
              <span class="tool-list small">
                <code v-for="t in g.tools" :key="t.tool" :class="{ 'err-text': t.failed }" :title="t.failed ? `${t.failed} of ${t.calls} failed` : ''">{{ t.tool }}<template v-if="t.calls > 1"> ×{{ t.calls }}</template></code>
              </span>
            </div>
          </div>
        </div>
      </section>

      <section class="card activity">
        <header class="card-header">
          <h2>Activity</h2>
          <button v-if="picked" type="button" class="btn btn-ghost btn-sm" data-testid="skills-flow-all" @click="selected = null">all</button>
        </header>
        <div class="card-body">
          <select class="input input-sm" :value="session" data-testid="skills-flow-session" @change="emit('session', ($event.target as HTMLSelectElement).value)">
            <option value="">Every session</option>
            <option v-for="s in activity?.sessions ?? []" :key="`${s.agent}/${s.session}`" :value="s.session">{{ sessionLabel(s) }}{{ s.ended ? ' (ended)' : '' }}</option>
          </select>
          <p v-if="picked" class="small picked-note">Only {{ picked.skill }}, hook {{ picked.hook.index + 1 }}</p>
          <p v-if="!shown.length" class="faint small" data-testid="skills-flow-quiet">Nothing recorded{{ picked ? ' for this hook' : '' }} yet.</p>
          <ol class="records">
            <li v-for="(r, i) in shown" :key="i" class="record" :class="{ failed: r.kind === 'tool' && !r.ok }" data-testid="skills-flow-record">
              <span class="t faint small" :title="new Date(r.at).toLocaleString()">{{ new Date(r.at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }}</span>
              <span class="dot" :class="dotClass(r)" />
              <span class="what">
                <span>{{ recordText(r, skillOf).title }}</span>
                <span class="faint small">{{ recordText(r, skillOf).detail }}</span>
              </span>
            </li>
          </ol>
          <p class="faint small foot">Only what happened: never a prompt, a hook's text, a tool's arguments or its answer. Kept on this machine for a week.</p>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.flow-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 320px;
  gap: 12px;
  align-items: start;
}
@media (max-width: 1100px) {
  .flow-grid {
    grid-template-columns: 1fr;
  }
}
.phases {
  display: grid;
  gap: 10px;
}
.phase {
  border-top: 2px solid var(--border-strong);
  padding-top: 8px;
  min-width: 0;
}
.phase h3 {
  margin: 0;
  font-size: 13px;
  display: flex;
  align-items: center;
  gap: 6px;
}
.num {
  display: inline-grid;
  place-items: center;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: var(--surface-hover);
  font-size: 11px;
}
.trigger {
  margin: 2px 0 8px;
  min-height: 32px;
}
.group + .group {
  margin-top: 8px;
}
.pattern {
  display: block;
  font-size: 11px;
  margin-bottom: 4px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.node {
  --c: var(--accent);
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 1px;
  width: 100%;
  margin-bottom: 6px;
  padding: 7px 9px;
  border: 1px solid var(--border);
  border-left: 3px solid var(--c);
  border-radius: 8px;
  background: var(--surface);
  color: var(--text);
  text-align: left;
  cursor: pointer;
}
.node:hover,
.node:focus-visible {
  border-color: var(--c);
}
.node.picked {
  box-shadow: 0 0 0 1px var(--c);
}
.node.off {
  border-left-style: dashed;
  opacity: 0.6;
}
.fired {
  color: var(--c);
}
.node.off .fired {
  color: var(--text-muted);
}
.tip {
  display: none;
  position: absolute;
  z-index: 20;
  left: 0;
  top: calc(100% + 6px);
  width: 290px;
  padding: 10px;
  border: 1px solid var(--border-strong);
  border-radius: 8px;
  background: var(--bg-elev);
  box-shadow: var(--shadow-lg);
  font-size: 12px;
  cursor: default;
}
.node:hover .tip,
.node:focus-visible .tip {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.tip-title {
  font-weight: 600;
  margin-bottom: 3px;
}
.tip-row {
  display: grid;
  grid-template-columns: 74px 1fr;
  gap: 8px;
}
.tip-row .k {
  color: var(--text-muted);
}
.tip-text {
  margin-top: 4px;
  padding-left: 7px;
  border-left: 2px solid var(--c);
  color: var(--text-muted);
  font-style: italic;
}
.c0 {
  --c: var(--accent);
}
.c1 {
  --c: var(--ok);
}
.c2 {
  --c: var(--info);
}
.c3 {
  --c: var(--warn);
}
.c4 {
  --c: var(--danger);
}
.c5 {
  --c: var(--neutral-text);
}
.left {
  margin: 8px 0 0;
}
.tools {
  margin-top: 12px;
  padding-top: 8px;
  border-top: 1px dashed var(--border);
}
.tools h3 {
  margin: 0 0 6px;
  font-size: 13px;
}
.tool-group {
  display: flex;
  gap: 10px;
  align-items: baseline;
  padding: 2px 0;
}
.tool-skill {
  flex: none;
  min-width: 140px;
  font-size: 12px;
  color: var(--c, var(--text-muted));
}
.tool-list {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 8px;
}
.err-text {
  color: var(--danger-text);
}
.warn-text {
  color: var(--warn-text);
}
.activity select {
  width: 100%;
  margin-bottom: 6px;
}
.picked-note {
  margin: 0 0 6px;
}
.records {
  list-style: none;
  margin: 0;
  padding: 0;
  max-height: 560px;
  overflow: auto;
}
.record {
  display: grid;
  grid-template-columns: 40px 12px 1fr;
  gap: 6px;
  align-items: start;
  padding: 5px 4px;
  border-radius: 6px;
}
.record:hover {
  background: var(--surface-hover);
}
.record.failed .what {
  color: var(--danger-text);
}
.dot {
  width: 9px;
  height: 9px;
  margin-top: 4px;
  border-radius: 50%;
  background: var(--c, var(--text-faint));
}
.dot.tool {
  background: var(--warn);
  border-radius: 2px;
}
.what {
  display: flex;
  flex-direction: column;
  min-width: 0;
  overflow-wrap: anywhere;
}
.foot {
  margin: 10px 0 0;
}
</style>
