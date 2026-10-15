import { fileURLToPath, URL } from 'node:url';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { VitePWA } from 'vite-plugin-pwa';

const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as { version: string };

export default defineConfig(({ mode }) => {
  // `--mode mock` or VITE_MOCK=1 swaps the real transport for the in-browser fake.
  const mock = mode === 'mock' || process.env.VITE_MOCK === '1';
  return {
    base: '/',
    define: {
      'import.meta.env.VITE_MOCK': JSON.stringify(mock ? '1' : ''),
      __APP_VERSION__: JSON.stringify(pkg.version),
    },
    resolve: {
      alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
    },
    server: {
      // `npm run dev` against a running hub: forward the API, keep the page on Vite.
      proxy: mock
        ? undefined
        : {
            '/hub': { target: 'http://127.0.0.1:4250', changeOrigin: false },
            '/auth': { target: 'http://127.0.0.1:4250', changeOrigin: false },
          },
    },
    plugins: [
      vue(),
      VitePWA({
        registerType: 'autoUpdate',
        injectRegister: null,
        includeAssets: ['favicon.svg', 'icon.svg', 'icon-maskable.svg'],
        manifest: {
          name: 'devstack',
          short_name: 'devstack',
          description: 'Dashboard for devstack: services, candidates, logs and resources of your local stacks.',
          start_url: '/',
          scope: '/',
          display: 'standalone',
          background_color: '#0f1115',
          theme_color: '#0f1115',
          icons: [
            { src: 'icon.svg', sizes: 'any', type: 'image/svg+xml', purpose: 'any' },
            { src: 'icon-maskable.svg', sizes: 'any', type: 'image/svg+xml', purpose: 'maskable' },
          ],
        },
        workbox: {
          globPatterns: ['**/*.{js,css,html,svg,woff2}'],
          navigateFallback: '/index.html',
          // API, pairing and streams always go to the network.
          navigateFallbackDenylist: [/^\/hub\//, /^\/auth/],
          cleanupOutdatedCaches: true,
        },
        devOptions: { enabled: false },
      }),
    ],
  };
});
