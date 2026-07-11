import { build } from "esbuild";
import { mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const frontendRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
process.chdir(frontendRoot);

const outfile = resolve(frontendRoot, "node_modules/.cache/coder/workflowSpecAdapter.test.mjs");
mkdirSync(dirname(outfile), { recursive: true });

await build({
  entryPoints: [resolve(frontendRoot, "src/workflowSpecAdapter.test.ts")],
  outfile,
  bundle: true,
  platform: "node",
  format: "esm",
  sourcemap: "inline",
  logLevel: "silent"
});

await import(pathToFileURL(outfile).href);
