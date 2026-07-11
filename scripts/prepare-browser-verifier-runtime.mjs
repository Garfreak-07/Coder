#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const repoRoot = path.resolve(path.dirname(scriptPath), "..");

const args = parseArgs(process.argv.slice(2));
const runtimeRoot = resolveRuntimeRoot(args);
const npmCache = path.resolve(args.npmCache ?? path.join(runtimeRoot, "npm-cache"));
const browsersPath = path.resolve(
  args.browsersPath ??
    process.env.CODER_PLAYWRIGHT_BROWSERS_PATH ??
    process.env.PLAYWRIGHT_BROWSERS_PATH ??
    path.join(runtimeRoot, "ms-playwright"),
);
const packageSpec = args.packageSpec ?? "playwright";

if (args.help) {
  printUsage();
  process.exit(0);
}

if (args.installBrowsers) {
  args.install = true;
}

try {
  if (args.install) {
    installPackage();
  }
  if (args.installBrowsers) {
    installBrowsers();
  }
  const status = collectStatus();
  printStatus(status);
  if (args.failIfMissing && status.status !== "ready") {
    process.exit(1);
  }
} catch (error) {
  const status = collectStatus();
  status.status = "error";
  status.error = error instanceof Error ? error.message : String(error);
  printStatus(status);
  process.exit(1);
}

function parseArgs(rawArgs) {
  const parsed = {
    check: false,
    failIfMissing: false,
    help: false,
    install: false,
    installBrowsers: false,
    json: false,
    runtimeRoot: null,
    npmCache: null,
    browsersPath: null,
    packageSpec: null,
  };

  for (let index = 0; index < rawArgs.length; index += 1) {
    const arg = rawArgs[index];
    switch (arg) {
      case "--check":
        parsed.check = true;
        break;
      case "--fail-if-missing":
        parsed.failIfMissing = true;
        break;
      case "--help":
      case "-h":
        parsed.help = true;
        break;
      case "--install":
        parsed.install = true;
        break;
      case "--install-browsers":
      case "--with-browsers":
        parsed.installBrowsers = true;
        break;
      case "--json":
        parsed.json = true;
        break;
      case "--runtime-root":
        parsed.runtimeRoot = requireValue(rawArgs, ++index, arg);
        break;
      case "--npm-cache":
        parsed.npmCache = requireValue(rawArgs, ++index, arg);
        break;
      case "--browsers-path":
        parsed.browsersPath = requireValue(rawArgs, ++index, arg);
        break;
      case "--package":
        parsed.packageSpec = requireValue(rawArgs, ++index, arg);
        break;
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return parsed;
}

function requireValue(rawArgs, index, flag) {
  const value = rawArgs[index];
  if (!value || value.startsWith("--")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function resolveRuntimeRoot(parsed) {
  if (parsed.runtimeRoot) {
    return path.resolve(parsed.runtimeRoot);
  }
  if (process.env.CODER_BROWSER_VERIFIER_RUNTIME_DIR) {
    return path.resolve(process.env.CODER_BROWSER_VERIFIER_RUNTIME_DIR);
  }
  if (process.env.CODER_RUNTIME_CACHE_DIR) {
    return path.resolve(process.env.CODER_RUNTIME_CACHE_DIR, "browser-verifier");
  }
  if (process.env.CODER_CACHE_DIR) {
    return path.resolve(process.env.CODER_CACHE_DIR, "runtime", "browser-verifier");
  }
  return path.resolve(repoRoot, "tmp", "coder-runtime-cache", "browser-verifier");
}

function installPackage() {
  fs.mkdirSync(runtimeRoot, { recursive: true });
  fs.mkdirSync(npmCache, { recursive: true });
  runNpm([
    "--prefix",
    runtimeRoot,
    "install",
    "--no-audit",
    "--no-fund",
    "--save-exact",
    packageSpec,
  ], {
    PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: "1",
  });
}

function installBrowsers() {
  fs.mkdirSync(browsersPath, { recursive: true });
  runNpm([
    "--prefix",
    runtimeRoot,
    "exec",
    "--",
    "playwright",
    "install",
    "chromium",
  ], {
    PLAYWRIGHT_BROWSERS_PATH: browsersPath,
  });
}

function runNpm(npmArgs, extraEnv = {}) {
  const npm = npmInvocation();
  const fullArgs = [...npm.args, ...npmArgs];
  const result = spawnSync(npm.command, fullArgs, {
    cwd: repoRoot,
    env: {
      ...process.env,
      npm_config_cache: npmCache,
      ...extraEnv,
    },
    stdio: "inherit",
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`${npm.display} ${npmArgs.join(" ")} failed with exit code ${result.status}`);
  }
}

function npmInvocation() {
  const npmCli = path.join(path.dirname(process.execPath), "node_modules", "npm", "bin", "npm-cli.js");
  if (fs.existsSync(npmCli)) {
    return {
      command: process.execPath,
      args: [npmCli],
      display: "npm",
    };
  }
  const command = os.platform() === "win32" ? "npm.cmd" : "npm";
  return {
    command,
    args: [],
    display: command,
  };
}

function collectStatus() {
  const nodePath = process.execPath;
  const nodeModules = path.join(runtimeRoot, "node_modules");
  const packageJson = path.join(nodeModules, "playwright", "package.json");
  const packageInstalled = fs.existsSync(packageJson);
  const browserEntries = browserCacheEntries(browsersPath);
  const chromiumInstalled = browserEntries.some((entry) => entry.startsWith("chromium"));
  const status = packageInstalled ? "ready" : "missing_playwright";
  return {
    status,
    runtime_root: runtimeRoot,
    npm_cache: npmCache,
    browsers_path: browsersPath,
    node_path: nodePath,
    node_modules: nodeModules,
    playwright_package: packageJson,
    package_installed: packageInstalled,
    chromium_installed: chromiumInstalled,
    browser_entries: browserEntries,
    install_performed: args.install,
    browser_install_performed: args.installBrowsers,
    note: chromiumInstalled
      ? "Playwright package and owned Chromium cache are present."
      : "Playwright package is enough when Chrome or Edge is installed; pass --install-browsers to cache Chromium under browsers_path.",
  };
}

function browserCacheEntries(root) {
  try {
    return fs
      .readdirSync(root, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name)
      .sort();
  } catch {
    return [];
  }
}

function printStatus(status) {
  if (args.json) {
    console.log(JSON.stringify(status, null, 2));
    return;
  }
  console.log(`Browser verifier runtime: ${status.status}`);
  console.log(`Runtime root: ${status.runtime_root}`);
  console.log(`npm cache: ${status.npm_cache}`);
  console.log(`Browsers path: ${status.browsers_path}`);
  console.log(`Node: ${status.node_path}`);
  console.log(`Playwright package: ${status.package_installed ? status.playwright_package : "missing"}`);
  console.log(`Owned Chromium cache: ${status.chromium_installed ? "present" : "missing"}`);
  console.log(status.note);
  if (status.error) {
    console.error(`Error: ${status.error}`);
  }
}

function printUsage() {
  console.log(`Usage: node scripts/prepare-browser-verifier-runtime.mjs [options]

Checks or prepares Coder's owned Playwright runtime for browser verification.
Default mode is check-only and performs no network or installation.

Options:
  --check                    Check runtime state only.
  --fail-if-missing          Exit 1 when Playwright is missing.
  --install                  Install the Playwright npm package only.
  --install-browsers         Install Chromium into the owned browsers path.
  --runtime-root <path>      Override runtime root.
  --npm-cache <path>         Override npm cache path.
  --browsers-path <path>     Override PLAYWRIGHT_BROWSERS_PATH.
  --package <spec>           Override npm package spec, default: playwright.
  --json                     Print machine-readable JSON.
  --help                     Show this help.
`);
}
