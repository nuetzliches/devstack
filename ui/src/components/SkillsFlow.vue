<script setup lang="ts">
// The Skills page's flow (#482): when the hooks of the skills switched on give
// their text to the agents, when each was given last, and the tool calls
// through `devstack mcp` by the skill whose upstream they use. The sessions
// themselves, in time, are on a lane's Activity (view Timeline).
import { computed } from 'vue';
import type { ActivityRecord, ActivityView, PoolSkillsState } from '@/api/types';
import { conditionText, firings, flowNodes, onceText, phases, toolsByUpstream, type FlowNode } from '@/lib/flow';
import { ago } from '@/lib/format';

const props = defineProps<{
  state: PoolSkillsState | null;
  activity: ActivityView | null;
  now: number;
}>();

const nodes = computed(() => flowNodes(props.state));
const flow = computed(() => phases(nodes.value));
const records = computed<ActivityRecord[]>(() => props.activity?.records ?? []);
const tools = computed(() => toolsByUpstream(records.value));
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
                  :class="[`c${n.color}`, { off: !n.hook.applies }]"
                  :aria-describedby="`tip-${n.key}`"
                  data-testid="skills-flow-node"
                  :data-key="n.key"
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
          <p class="faint small left">The agents' sessions in time, with what devstack gave and the calls through it: a lane's Activity, view Timeline.</p>
          <p v-if="left.length" class="faint small left">Off, or in a set not accepted, and so not in the flow: {{ left.join(', ') }}.</p>

          <div v-if="tools.length" class="tools" data-testid="skills-flow-tools">
            <h3>Tools through devstack</h3>
            <div v-for="g in tools" :key="g.upstream ?? '-'" class="tool-group">
              <span class="tool-skill">
                <template v-if="g.skills.length">
                  <span v-for="(s, i) in g.skills" :key="s" :class="colorOf(s) !== undefined ? `c${colorOf(s)}` : ''">{{ s }}<template v-if="i < g.skills.length - 1">, </template></span>
                  <small class="muted"> · upstream {{ g.upstream }}</small>
                </template>
                <template v-else>tied to no skill</template>
              </span>
              <span class="tool-list small">
                <code v-for="t in g.tools" :key="t.tool" :class="{ 'err-text': t.failed }" :title="t.failed ? `${t.failed} of ${t.calls} failed` : ''">{{ t.tool }}<template v-if="t.calls > 1"> ×{{ t.calls }}</template></code>
              </span>
            </div>
          </div>
        </div>
      </section>

    </div>
  </div>
</template>

<style scoped>
.flow-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 12px;
  align-items: start;
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
/* Dimmed by its parts, not `opacity` on the node: the tooltip inside it
   would take the opacity too and let the page show through. */
.node.off {
  border-left-style: dashed;
}
.node.off > :not(.tip) {
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
</style>
