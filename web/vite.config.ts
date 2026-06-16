import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Plain SPA. No backend dependency — the mockups run on in-memory data so the three design systems
// can be navigated and compared without the Rust server running.
export default defineConfig({
  plugins: [react()],
  server: { port: 5180 },
})
