import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Plain SPA on the live API. In dev the React app and the Rust API are separate processes; proxy the API paths to the running
// `kigumi serve` (default bind 127.0.0.1:8099) so the browser stays same-origin (no CORS needed).
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5180,
    proxy: {
      '/api': 'http://127.0.0.1:8099',
      '/auth': 'http://127.0.0.1:8099',
      '/openapi.json': 'http://127.0.0.1:8099',
    },
  },
})
