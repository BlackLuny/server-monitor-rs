import { sveltekit } from '@sveltejs/kit/vite';
import type { ProxyOptions } from 'vite';
import { defineConfig } from 'vite';

// In production the panel serves the SPA itself, so the browser's `Origin`
// always matches the request's `Host` and the panel's CSRF middleware
// passes. During dev with this proxy the browser hits Vite (5174) while
// the upstream is the panel (8080); without rewriting `Origin` the CSRF
// check rejects every mutating request as cross-origin.
const PANEL_HTTP = 'http://127.0.0.1:8080';
const rewriteOrigin: ProxyOptions = {
  target: PANEL_HTTP,
  changeOrigin: true,
  configure: (proxy) => {
    proxy.on('proxyReq', (proxyReq) => {
      proxyReq.setHeader('origin', PANEL_HTTP);
    });
  }
};

export default defineConfig({
  plugins: [sveltekit()],
  server: {
    port: 5173,
    // Proxy API + WS calls to the panel running on :8080 during development.
    proxy: {
      '/api': rewriteOrigin,
      '/healthz': rewriteOrigin,
      '/ws': {
        target: 'ws://127.0.0.1:8080',
        ws: true,
        changeOrigin: true
      }
    }
  }
});
