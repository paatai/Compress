import path from 'node:path'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

export default defineConfig({
  base: './',
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { '@': path.resolve(__dirname, './src') },
  },
  worker: { format: 'es' },
  optimizeDeps: { exclude: ['@jsquash/webp', '@jsquash/jxl'] },
  server: { port: 47123, host: '127.0.0.1' },
  preview: { port: 47124, host: '127.0.0.1' },
})
