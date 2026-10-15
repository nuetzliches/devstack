import { createApp } from 'vue';
import { createPinia } from 'pinia';
import App from './App.vue';
import { router } from './router';
import { initTransport, isMock } from './api';
import './styles/tokens.css';
import './styles/base.css';
import './styles/components.css';

async function start() {
  await initTransport();
  const app = createApp(App);
  app.use(createPinia());
  app.use(router);
  app.mount('#app');

  // The service worker caches the app shell so the page opens without the hub.
  // Not in mock builds served by the dev server.
  if (import.meta.env.PROD && 'serviceWorker' in navigator && !isMock) {
    const { registerSW } = await import('virtual:pwa-register');
    registerSW({ immediate: true });
  }
}

void start();
