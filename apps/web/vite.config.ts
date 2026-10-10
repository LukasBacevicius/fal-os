import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Port 5173 (8787-8789 are used by the engine spike and the host).
export default defineConfig({
  plugins: [react()],
  server: { port: 5173, strictPort: true, host: '127.0.0.1' },
  preview: { port: 5174, host: '127.0.0.1' },
  build: { target: 'es2022', outDir: 'dist' },
  optimizeDeps: { exclude: ['./src/wasm/pkg'] },
  clearScreen: false,
});
