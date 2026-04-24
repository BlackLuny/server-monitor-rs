// Pure SPA mode: SvelteKit produces a single index.html + hashed assets and
// the panel's rust-embed fallback serves index.html for any unknown route.
// No SSR, no pre-render — all data comes from the panel's REST API at runtime.
export const ssr = false;
export const prerender = false;
export const trailingSlash = 'never';
