import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      // Single-page app: SvelteKit produces `build/index.html` + hashed assets
      // and the panel's rust-embed fallback serves index.html for any unknown
      // route, so client-side routing works out of the box.
      fallback: 'index.html',
      strict: false
    }),
    // We want every route to pre-render to the SPA shell; actual data comes
    // from the REST API at runtime.
    prerender: {
      handleHttpError: 'warn',
      entries: []
    }
  }
};

export default config;
