import { createRouter, createWebHashHistory } from 'vue-router';
import { persist } from './lib/persist';

// Hash history: the hub serves the shell at `/` only, and the service worker's
// navigation fallback stays trivial; deep links still work offline.
export const router = createRouter({
  history: createWebHashHistory('/'),
  routes: [
    { path: '/', name: 'home', component: () => import('./views/HomeView.vue') },
    {
      path: '/s/:stack',
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
