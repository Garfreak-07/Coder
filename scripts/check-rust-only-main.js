#!/usr/bin/env node

const fs = require("fs");
const path = require("path");

const repoRoot = path.resolve(__dirname, "..");
const failures = [];

function rel(...parts) {
  return path.join(repoRoot, ...parts);
}

function assertMissing(relativePath, label) {
  if (fs.existsSync(rel(relativePath))) {
    failures.push(`${label} must not exist: ${relativePath}`);
  }
}

function walkFiles(directory) {
  if (!fs.existsSync(directory)) return [];
  const entries = fs.readdirSync(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    if (entry.name === "node_modules" || entry.name === "dist") continue;
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...walkFiles(absolute));
    } else if (entry.isFile()) {
      files.push(absolute);
    }
  }
  return files;
}

function assertNoPatterns(files, patterns) {
  for (const file of files) {
    const text = fs.readFileSync(file, "utf8");
    const relative = path.relative(repoRoot, file).replaceAll(path.sep, "/");
    for (const pattern of patterns) {
      if (text.includes(pattern)) {
        failures.push(`${relative} still contains ${JSON.stringify(pattern)}`);
      }
    }
  }
}

assertMissing(["legacy", "python"].join("-"), "Removed compatibility implementation");
assertMissing("pyproject.toml", "Root Python package metadata");

assertNoPatterns(walkFiles(rel("frontend")), [
  "/api/" + "v2",
  "CODER_USE_" + "RUST_API",
  "VITE_CODER_" + "API_VERSION",
  "coder_" + "api_version",
  "api_" + "version=v2",
  "rust_" + "api"
]);

assertNoPatterns([rel(".github", "workflows", "ci.yml")], [
  "Legacy " + "Python compatibility",
  ["legacy", "python"].join("-")
]);

if (failures.length > 0) {
  console.error("Rust-only main guard failed:");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log("Rust-only main guard passed.");
