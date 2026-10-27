import { createRouter, createWebHashHistory } from 'vue-router';

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
        { path: 'events', name: 'events', component: () => import('./views/EventsView.vue'), meta: { title: 'Events' } },
      ],
    },
    { path: '/pool', name: 'pool', component: () => import('./views/PoolView.vue'), meta: { title: 'Pool' } },
    { path: '/upstreams', name: 'upstreams', component: () => import('./views/UpstreamsView.vue'), meta: { title: 'MCP upstreams' } },
    { path: '/:pathMatch(.*)*', redirect: '/' },
  ],
  scrollBehavior(to, from) {
    if (to.path === from.path) return false;
    return { top: 0 };
  },
});
