import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

/**
 * Dev only: serves the local fal-os host's discovery file (~/.fal-os/host.json, or
 * $FALOS_HOME/host.json) at /__falos/host so the page can find the host and its token.
 * `configureServer` never runs in `vite build`, and only loopback clients get an answer.
 */
function falosHost(): Plugin {
  return {
    name: 'falos-host-discovery',
    configureServer(server) {
      server.middlewares.use('/__falos/host', (req, res) => {
        const ip = req.socket.remoteAddress ?? '';
        if (!['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(ip)) {
          res.statusCode = 403;
          return res.end();
        }
        try {
          const home = process.env.FALOS_HOME || join(homedir(), '.fal-os');
          const h = JSON.parse(readFileSync(join(home, 'host.json'), 'utf8'));
          res.setHeader('content-type', 'application/json');
          res.setHeader('cache-control', 'no-store');
          res.end(JSON.stringify({ port: h.port, token: h.token, project: h.project }));
        } catch {
          res.statusCode = 404;
          res.end();
        }
      });
    },
  };
}

// Port 5173 (8787-8789 are used by the engine spike and the host). FALOS_WEB_PORT overrides.
export default defineConfig({
  plugins: [react(), falosHost()],
  server: { port: Number(process.env.FALOS_WEB_PORT) || 5173, strictPort: true, host: '127.0.0.1' },
  preview: { port: 5174, host: '127.0.0.1' },
  build: { target: 'es2022', outDir: 'dist' },
  optimizeDeps: { exclude: ['./src/wasm/pkg'] },
  clearScreen: false,
});
