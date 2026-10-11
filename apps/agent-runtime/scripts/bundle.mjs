// Bundles the runtime (src/main.ts) and the first-run setup (src/setup.ts) into single ESM files in
// dist/ for the packaged desktop app, which runs them with a bundled Node (no tsx, no node_modules).
//
// - Everything is inlined, including @anthropic-ai/claude-agent-sdk, @fal-ai/client and
//   @openai/codex-sdk. Their native CLIs are not: the runtime uses the user's own `claude`
//   (`pathToClaudeCodeExecutable`) and `codex` (`codexPathOverride`) binaries.
// - Paths resolved from import.meta.url (`../motion-template/`, `../scripts/…`) keep working
//   because dist/ sits next to them in the packaged layout.
import { build } from "esbuild";

const common = {
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node22",
  sourcemap: "linked",
  legalComments: "linked",
  logLevel: "warning",
  // CJS dependencies call require(); give the ESM bundle one.
  banner: { js: "import { createRequire as __falosRequire } from 'node:module'; const require = __falosRequire(import.meta.url);" },
};

await build({ ...common, entryPoints: { main: "src/main.ts", setup: "src/setup.ts" }, outdir: "dist", outExtension: { ".js": ".mjs" } });
console.log("bundled dist/main.mjs, dist/setup.mjs");
