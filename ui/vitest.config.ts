import { fileURLToPath, URL } from 'node:url';
import { defineConfig } from 'vitest/config';
import vue from '@vitejs/plugin-vue';

// Unit tests (`npm test`): the lib functions and the Pinia stores, against a
// fake transport. Not the PWA build, so not `vite.config.ts`.
export default defineConfig({
  plugins: [vue()],
  define: {
    'import.meta.env.VITE_MOCK': JSON.stringify(''),
    __APP_VERSION__: JSON.stringify('0.0.0-test'),
  },
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  test: {
    environment: 'happy-dom',
    include: ['src/**/*.test.ts'],
  },
});
