import { createRouter, createWebHashHistory } from 'vue-router';
import type { RouteLocation } from 'vue-router';
import { persist } from './lib/persist';
import { oldLaneLink } from './lib/stackRoutes';

function oldLink(to: Pick<RouteLocation, 'params' | 'query'>) {
  const stack = String(to.params.stack);
  const rest = Array.isArray(to.params.rest) ? to.params.rest.join('/') : String(to.params.rest ?? '');
  return oldLaneLink(stack, rest, to.query, persist.get<string | null>(`lane.${stack}`));
}

// Hash history: the hub serves the shell at `/` only, and the service worker's
// navigation fallback stays trivial; deep links still work offline.
export const router = createRouter({
  history: createWebHashHistory('/'),
  routes: [
    { path: '/', name: 'home', component: () => import('./views/HomeView.vue') },
    // A stack's page, above its lanes (ADR-0081). An older link with
    // `?lane=` goes to that lane's view.
    {
      path: '/s/:stack',
      name: 'stack',
      component: () => import('./views/StackPage.vue'),
      meta: { title: 'Stack' },
      beforeEnter: (to) => oldLink(to) ?? true,
    },
    // A lane's views, the main working tree's as the lane `main` (ADR-0080).
    {
      path: '/s/:stack/l/:lane',
      component: () => import('./views/StackLayout.vue'),
      children: [
        // The overview is a list with a detail: picking a service or a
        // container keeps the list and shows it on the right.
        {
          path: '',
          component: () => import('./views/OverviewView.vue'),
          children: [
            { path: '', name: 'overview', component: () => import('./views/OverviewHome.vue'), meta: { title: 'Overview' } },
            { path: 'services/:name', name: 'service', component: () => import('./views/ServiceDetailView.vue'), meta: { title: 'Service' } },
            { path: 'infra/:group/:service', name: 'container', component: () => import('./views/ContainerDetailView.vue'), meta: { title: 'Container' } },
          ],
        },
        { path: 'infra', name: 'infra', component: () => import('./views/InfraView.vue'), meta: { title: 'Infrastructure' } },
        { path: 'commands', name: 'commands', component: () => import('./views/CommandsView.vue'), meta: { title: 'Commands' } },
        { path: 'usage', name: 'usage', component: () => import('./views/UsageView.vue'), meta: { title: 'Usage' } },
        { path: 'events', name: 'events', component: () => import('./views/EventsView.vue'), meta: { title: 'Activity' } },
      ],
    },
    // Links from before ADR-0081 (`#/s/<stack>/services/api?lane=feat`): the
    // same tab of that lane, else of the lane shown last, else of `main`.
    { path: '/s/:stack/:rest(.*)+', redirect: (to) => oldLink(to) ?? { name: 'stack', params: { stack: to.params.stack } } },
    // The pool area: `#/pool` opens the pool shown last (notifications and
    // `devstack://open?page=pool` link there); the pool and the tab are in
    // the URL from then on. What holds for this machine in every pool has a
    // place of its own (a static segment ranks above `:pool`).
    { path: '/pool', name: 'pool', redirect: () => ({ name: 'pool-overview', params: { pool: persist.get<string>('pool.current') ?? 'default' } }) },
    { path: '/pool/this-machine', name: 'pool-here', component: () => import('./views/pool/ThisMachineView.vue'), meta: { title: 'This machine' } },
    {
      path: '/pool/:pool',
      component: () => import('./views/pool/PoolLayout.vue'),
      children: [
        { path: '', name: 'pool-overview', component: () => import('./views/pool/PoolOverview.vue'), meta: { title: 'Pool' } },
        { path: 'machines', name: 'pool-machines', component: () => import('./views/pool/PoolMachines.vue'), meta: { title: 'Machines' } },
        { path: 'machines/:id', name: 'pool-machine', component: () => import('./views/pool/PoolMachine.vue'), meta: { title: 'Machine' } },
        { path: 'lanes', name: 'pool-lanes', component: () => import('./views/pool/PoolLanes.vue'), meta: { title: 'Lanes' } },
        { path: 'admin', name: 'pool-admin', component: () => import('./views/pool/PoolAdmin.vue'), meta: { title: 'Admin' } },
      ],
    },
    { path: '/upstreams', name: 'upstreams', component: () => import('./views/UpstreamsView.vue'), meta: { title: 'MCP upstreams' } },
    { path: '/skills', name: 'skills', component: () => import('./views/SkillsView.vue'), meta: { title: 'Skills' } },
    { path: '/:pathMatch(.*)*', redirect: '/' },
  ],
  scrollBehavior(to, from) {
    if (to.path === from.path) return false;
    return { top: 0 };
  },
});
