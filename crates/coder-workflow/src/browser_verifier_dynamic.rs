use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::{process::Command, time as tokio_time};

use crate::{
    browser_verifier::{
        browser_any_file_exists, browser_verifier_is_dynamic_check, BrowserVerifierCheck,
        BrowserVerifierCheckStatus,
    },
    truncate_public,
};

pub(crate) struct BrowserDynamicRunInput {
    pub(crate) run_id: String,
    pub(crate) repo_root: String,
    pub(crate) runtime_root: PathBuf,
    pub(crate) task: String,
    pub(crate) selected_checks: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BrowserVerifierRuntimeStatus {
    pub runtime_root: PathBuf,
    pub browsers_path: PathBuf,
    pub node_path: Option<PathBuf>,
    pub resolved_node_modules: Option<PathBuf>,
    pub candidates: Vec<BrowserVerifierPlaywrightCandidate>,
}

#[derive(Debug, Clone)]
pub struct BrowserVerifierPlaywrightCandidate {
    pub source: String,
    pub path: PathBuf,
    pub path_exists: bool,
    pub has_playwright_package: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct BrowserDynamicRunOutput {
    pub(crate) checks: Vec<BrowserVerifierCheck>,
    pub(crate) evidence: Value,
}

#[async_trait]
pub(crate) trait BrowserDynamicRunner: Send + Sync {
    async fn run(&self, input: BrowserDynamicRunInput) -> BrowserDynamicRunOutput;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProcessBrowserDynamicRunner;

#[async_trait]
impl BrowserDynamicRunner for ProcessBrowserDynamicRunner {
    async fn run(&self, input: BrowserDynamicRunInput) -> BrowserDynamicRunOutput {
        run_process_browser_dynamic_checks(input).await
    }
}

async fn run_process_browser_dynamic_checks(
    input: BrowserDynamicRunInput,
) -> BrowserDynamicRunOutput {
    let requested_checks = input
        .selected_checks
        .iter()
        .filter(|check| browser_verifier_is_dynamic_check(check))
        .cloned()
        .collect::<Vec<_>>();
    let base_evidence = json!({
        "runner": "process-playwright",
        "requested_checks": requested_checks,
        "task": input.task.clone(),
        "repo_root": input.repo_root.clone()
    });

    let entry_path = match browser_dynamic_entry_path(&input.repo_root) {
        Ok(path) => path,
        Err(reason) => {
            return BrowserDynamicRunOutput {
                checks: vec![BrowserVerifierCheck::blocked(
                    "browser_dynamic.entry",
                    reason.clone(),
                )],
                evidence: merge_browser_dynamic_evidence(
                    base_evidence,
                    json!({"status": "blocked", "reason": reason}),
                ),
            }
        }
    };
    let node_path = match find_node_executable() {
        Some(path) => path,
        None => {
            let reason =
                "Node.js was not found on PATH; browser verification needs Node to run Playwright";
            return BrowserDynamicRunOutput {
                checks: vec![BrowserVerifierCheck::blocked(
                    "browser_dynamic.node",
                    reason,
                )],
                evidence: merge_browser_dynamic_evidence(
                    base_evidence,
                    json!({"status": "blocked", "reason": reason}),
                ),
            };
        }
    };
    let node_modules = match find_playwright_node_modules(&input.repo_root, &input.runtime_root) {
        Some(path) => path,
        None => {
            let reason = "Playwright was not configured for Coder browser verification; configure CODER_PLAYWRIGHT_NODE_MODULES, CODER_BROWSER_VERIFIER_RUNTIME_DIR, CODER_RUNTIME_CACHE_DIR, or Coder's store runtime cache. Do not install Playwright into the target project solely for Coder verification.";
            return BrowserDynamicRunOutput {
                checks: vec![BrowserVerifierCheck::blocked(
                    "browser_dynamic.playwright",
                    reason,
                )],
                evidence: merge_browser_dynamic_evidence(
                    base_evidence,
                    json!({
                        "status": "blocked",
                        "reason": reason,
                        "runtime_root": input.runtime_root.display().to_string(),
                        "searched_node_modules": playwright_node_modules_candidates(&input.repo_root, &input.runtime_root)
                            .into_iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                    }),
                ),
            };
        }
    };

    let script_path = browser_dynamic_script_path(&input.run_id, &input.runtime_root);
    if let Some(script_dir) = script_path.parent() {
        if let Err(error) = fs::create_dir_all(script_dir) {
            let reason = format!(
                "failed to create browser verifier script directory at {}: {error}",
                script_dir.display()
            );
            return BrowserDynamicRunOutput {
                checks: vec![BrowserVerifierCheck::blocked(
                    "browser_dynamic.script",
                    reason.clone(),
                )],
                evidence: merge_browser_dynamic_evidence(
                    base_evidence,
                    json!({"status": "blocked", "reason": reason}),
                ),
            };
        }
    }
    if let Err(error) = fs::write(&script_path, BROWSER_DYNAMIC_PLAYWRIGHT_SCRIPT) {
        let reason = format!(
            "failed to write ephemeral browser verifier script at {}: {error}",
            script_path.display()
        );
        return BrowserDynamicRunOutput {
            checks: vec![BrowserVerifierCheck::blocked(
                "browser_dynamic.script",
                reason.clone(),
            )],
            evidence: merge_browser_dynamic_evidence(
                base_evidence,
                json!({"status": "blocked", "reason": reason}),
            ),
        };
    }

    let selected_json =
        serde_json::to_string(&input.selected_checks).unwrap_or_else(|_| "[]".into());
    let command_display = format!(
        "{} {} {} {} <selected-checks>",
        node_path.display(),
        script_path.display(),
        entry_path.display(),
        node_modules.display()
    );
    let mut command = Command::new(&node_path);
    let browsers_path = browser_verifier_browsers_path(&input.runtime_root);
    command
        .arg(&script_path)
        .arg(&entry_path)
        .arg(&node_modules)
        .arg(selected_json)
        .current_dir(&input.repo_root)
        .env("PLAYWRIGHT_BROWSERS_PATH", &browsers_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = match tokio_time::timeout(Duration::from_secs(60), command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            let reason = format!("failed to run browser verifier command: {error}");
            let _ = fs::remove_file(&script_path);
            return BrowserDynamicRunOutput {
                checks: vec![BrowserVerifierCheck::blocked(
                    "browser_dynamic.command",
                    reason.clone(),
                )],
                evidence: merge_browser_dynamic_evidence(
                    base_evidence,
                    json!({
                        "status": "blocked",
                        "reason": reason,
                        "command": command_display
                    }),
                ),
            };
        }
        Err(_) => {
            let reason = "browser verifier command timed out after 60 seconds";
            let _ = fs::remove_file(&script_path);
            return BrowserDynamicRunOutput {
                checks: vec![BrowserVerifierCheck::fail(
                    "browser_dynamic.timeout",
                    reason,
                )],
                evidence: merge_browser_dynamic_evidence(
                    base_evidence,
                    json!({
                        "status": "failed",
                        "reason": reason,
                        "command": command_display
                    }),
                ),
            };
        }
    };
    let _ = fs::remove_file(&script_path);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let payload = match browser_dynamic_output_json(&stdout) {
        Ok(payload) => payload,
        Err(reason) => {
            return BrowserDynamicRunOutput {
                checks: vec![BrowserVerifierCheck::fail(
                    "browser_dynamic.output",
                    reason.clone(),
                )],
                evidence: merge_browser_dynamic_evidence(
                    base_evidence,
                    json!({
                        "status": "failed",
                        "reason": reason,
                        "command": command_display,
                        "exit_code": output.status.code(),
                        "stdout": truncate_public(&stdout, 4000),
                        "stderr": truncate_public(&stderr, 4000)
                    }),
                ),
            }
        }
    };
    let mut checks = browser_dynamic_checks_from_payload(&payload);
    if checks.is_empty() {
        checks.push(if output.status.success() {
            BrowserVerifierCheck::pass(
                "browser_dynamic.runner",
                "Playwright verifier completed without per-check output",
            )
        } else {
            BrowserVerifierCheck::fail(
                "browser_dynamic.runner",
                "Playwright verifier failed without per-check output",
            )
        });
    }
    if !output.status.success()
        && checks
            .iter()
            .all(|check| check.status == BrowserVerifierCheckStatus::Passed)
    {
        checks.push(BrowserVerifierCheck::fail(
            "browser_dynamic.command",
            "Playwright verifier exited unsuccessfully",
        ));
    }

    BrowserDynamicRunOutput {
        checks,
        evidence: merge_browser_dynamic_evidence(
            base_evidence,
            json!({
                "status": payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or(if output.status.success() { "ok" } else { "failed" }),
                "command": command_display,
                "entry": entry_path.display().to_string(),
                "node_modules": node_modules.display().to_string(),
                "exit_code": output.status.code(),
                "stdout": truncate_public(&stdout, 8000),
                "stderr": truncate_public(&stderr, 8000),
                "payload": payload
            }),
        ),
    }
}

fn merge_browser_dynamic_evidence(mut base: Value, extra: Value) -> Value {
    if let (Some(base_object), Some(extra_object)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra_object {
            base_object.insert(key.clone(), value.clone());
        }
    }
    base
}

fn browser_dynamic_entry_path(repo_root: &str) -> Result<PathBuf, String> {
    let root = Path::new(repo_root);
    let index = root.join("index.html");
    if index.is_file() {
        return Ok(index);
    }
    if browser_any_file_exists(
        repo_root,
        &[
            "src/main.js",
            "src/main.jsx",
            "src/main.tsx",
            "src/App.jsx",
            "src/App.tsx",
            "app/page.tsx",
            "pages/index.tsx",
        ],
    ) {
        return Err(
            "dynamic browser verification currently needs a static index.html entry; framework dev-server verification is not configured".to_owned(),
        );
    }
    Err("dynamic browser verification expected index.html in the project root".to_owned())
}

fn find_node_executable() -> Option<PathBuf> {
    if let Some(path) = env::var_os("CODER_NODE_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    find_executable_on_path(if cfg!(windows) {
        &["node.exe", "node.cmd", "node"]
    } else {
        &["node"]
    })
}

fn find_executable_on_path(names: &[&str]) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_playwright_node_modules(repo_root: &str, runtime_root: &Path) -> Option<PathBuf> {
    playwright_node_modules_candidate_entries(repo_root, runtime_root)
        .into_iter()
        .map(|candidate| candidate.path)
        .find(|path| path.join("playwright").join("package.json").is_file())
}

pub fn browser_verifier_runtime_status(
    repo_root: &str,
    runtime_root: &Path,
) -> BrowserVerifierRuntimeStatus {
    let candidates = playwright_node_modules_candidate_entries(repo_root, runtime_root)
        .into_iter()
        .map(|candidate| {
            let path_exists = candidate.path.exists();
            let has_playwright_package = candidate
                .path
                .join("playwright")
                .join("package.json")
                .is_file();
            BrowserVerifierPlaywrightCandidate {
                source: candidate.source,
                path: candidate.path,
                path_exists,
                has_playwright_package,
            }
        })
        .collect::<Vec<_>>();
    let resolved_node_modules = candidates
        .iter()
        .find(|candidate| candidate.has_playwright_package)
        .map(|candidate| candidate.path.clone());
    BrowserVerifierRuntimeStatus {
        runtime_root: runtime_root.to_path_buf(),
        browsers_path: browser_verifier_browsers_path(runtime_root),
        node_path: find_node_executable(),
        resolved_node_modules,
        candidates,
    }
}

fn browser_verifier_browsers_path(runtime_root: &Path) -> PathBuf {
    env::var_os("CODER_PLAYWRIGHT_BROWSERS_PATH")
        .or_else(|| env::var_os("PLAYWRIGHT_BROWSERS_PATH"))
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_root.join("ms-playwright"))
}

pub(crate) fn playwright_node_modules_candidates(
    repo_root: &str,
    runtime_root: &Path,
) -> Vec<PathBuf> {
    playwright_node_modules_candidate_entries(repo_root, runtime_root)
        .into_iter()
        .map(|candidate| candidate.path)
        .collect()
}

#[derive(Debug, Clone)]
struct PlaywrightNodeModulesCandidate {
    source: String,
    path: PathBuf,
}

fn playwright_node_modules_candidate_entries(
    repo_root: &str,
    runtime_root: &Path,
) -> Vec<PlaywrightNodeModulesCandidate> {
    let mut roots = Vec::new();
    if let Some(path) = env::var_os("CODER_PLAYWRIGHT_NODE_MODULES").map(PathBuf::from) {
        roots.push(PlaywrightNodeModulesCandidate {
            source: "env:CODER_PLAYWRIGHT_NODE_MODULES".to_owned(),
            path,
        });
    }
    if let Some(path) = env::var_os("CODER_BROWSER_VERIFIER_NODE_MODULES").map(PathBuf::from) {
        roots.push(PlaywrightNodeModulesCandidate {
            source: "env:CODER_BROWSER_VERIFIER_NODE_MODULES".to_owned(),
            path,
        });
    }
    for (source, root) in browser_runtime_roots(runtime_root) {
        push_browser_runtime_node_modules_candidates(&mut roots, &source, &root);
    }
    for (source, root) in browser_distribution_roots() {
        push_browser_runtime_node_modules_candidates(&mut roots, &source, &root);
    }
    let repo = PathBuf::from(repo_root);
    roots.push(PlaywrightNodeModulesCandidate::new(
        "fallback:repo_tmp_playwright_smoke",
        repo.join(".tmp")
            .join("playwright-smoke")
            .join("node_modules"),
    ));
    if let Ok(current) = env::current_dir() {
        roots.push(PlaywrightNodeModulesCandidate::new(
            "fallback:cwd_tmp_playwright_smoke",
            current
                .join(".tmp")
                .join("playwright-smoke")
                .join("node_modules"),
        ));
    }
    if let Some(manifest_root) = coder_manifest_workspace_root() {
        roots.push(PlaywrightNodeModulesCandidate::new(
            "fallback:workspace_tmp_playwright_smoke",
            manifest_root
                .join(".tmp")
                .join("playwright-smoke")
                .join("node_modules"),
        ));
    }
    roots.push(PlaywrightNodeModulesCandidate::new(
        "fallback:repo_node_modules",
        repo.join("node_modules"),
    ));
    roots.push(PlaywrightNodeModulesCandidate::new(
        "fallback:repo_frontend_node_modules",
        repo.join("frontend").join("node_modules"),
    ));
    if let Ok(current) = env::current_dir() {
        roots.push(PlaywrightNodeModulesCandidate::new(
            "fallback:cwd_node_modules",
            current.join("node_modules"),
        ));
        roots.push(PlaywrightNodeModulesCandidate::new(
            "fallback:cwd_frontend_node_modules",
            current.join("frontend").join("node_modules"),
        ));
    }
    if let Some(manifest_root) = coder_manifest_workspace_root() {
        roots.push(PlaywrightNodeModulesCandidate::new(
            "fallback:workspace_node_modules",
            manifest_root.join("node_modules"),
        ));
        roots.push(PlaywrightNodeModulesCandidate::new(
            "fallback:workspace_frontend_node_modules",
            manifest_root.join("frontend").join("node_modules"),
        ));
    }
    dedupe_candidates(roots)
}

impl PlaywrightNodeModulesCandidate {
    fn new(source: impl Into<String>, path: PathBuf) -> Self {
        Self {
            source: source.into(),
            path,
        }
    }
}

fn browser_runtime_roots(runtime_root: &Path) -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    if let Some(path) = env::var_os("CODER_BROWSER_VERIFIER_RUNTIME_DIR").map(PathBuf::from) {
        roots.push(("env:CODER_BROWSER_VERIFIER_RUNTIME_DIR".to_owned(), path));
    }
    if let Some(path) = env::var_os("CODER_RUNTIME_CACHE_DIR").map(PathBuf::from) {
        roots.push((
            "env:CODER_RUNTIME_CACHE_DIR/browser-verifier".to_owned(),
            path.join("browser-verifier"),
        ));
    }
    if let Some(path) = env::var_os("CODER_CACHE_DIR").map(PathBuf::from) {
        roots.push((
            "env:CODER_CACHE_DIR/runtime/browser-verifier".to_owned(),
            path.join("runtime").join("browser-verifier"),
        ));
    }
    roots.push(("store:runtime_root".to_owned(), runtime_root.to_path_buf()));
    roots
}

fn browser_distribution_roots() -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    if let Ok(exe) = env::current_exe() {
        for ancestor in exe.ancestors().take(5) {
            roots.push((
                "distribution:current_exe".to_owned(),
                ancestor.to_path_buf(),
            ));
        }
    }
    if let Some(manifest_root) = coder_manifest_workspace_root() {
        roots.push(("distribution:workspace_manifest".to_owned(), manifest_root));
    }
    roots
}

fn push_browser_runtime_node_modules_candidates(
    roots: &mut Vec<PlaywrightNodeModulesCandidate>,
    source: &str,
    root: &Path,
) {
    roots.push(PlaywrightNodeModulesCandidate::new(
        format!("{source}/node_modules"),
        root.join("node_modules"),
    ));
    roots.push(PlaywrightNodeModulesCandidate::new(
        format!("{source}/playwright/node_modules"),
        root.join("playwright").join("node_modules"),
    ));
    roots.push(PlaywrightNodeModulesCandidate::new(
        format!("{source}/playwright-smoke/node_modules"),
        root.join("playwright-smoke").join("node_modules"),
    ));
    roots.push(PlaywrightNodeModulesCandidate::new(
        format!("{source}/.tmp/playwright-smoke/node_modules"),
        root.join(".tmp")
            .join("playwright-smoke")
            .join("node_modules"),
    ));
    roots.push(PlaywrightNodeModulesCandidate::new(
        format!("{source}/vendor/playwright/node_modules"),
        root.join("vendor").join("playwright").join("node_modules"),
    ));
}

fn coder_manifest_workspace_root() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(option_env!("CARGO_MANIFEST_DIR")?);
    manifest_dir.parent()?.parent().map(Path::to_path_buf)
}

fn dedupe_candidates(
    candidates: Vec<PlaywrightNodeModulesCandidate>,
) -> Vec<PlaywrightNodeModulesCandidate> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for candidate in candidates {
        let key = candidate.path.display().to_string().to_ascii_lowercase();
        if seen.insert(key) {
            deduped.push(candidate);
        }
    }
    deduped
}

pub(crate) fn browser_dynamic_script_path(run_id: &str, runtime_root: &Path) -> PathBuf {
    let safe_run_id = run_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    runtime_root.join("browser-verifier-scripts").join(format!(
        "coder-browser-verifier-{}-{safe_run_id}.mjs",
        std::process::id()
    ))
}

fn browser_dynamic_output_json(stdout: &str) -> Result<Value, String> {
    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| "browser verifier produced no JSON output".to_owned())?;
    serde_json::from_str(json_line)
        .map_err(|error| format!("browser verifier produced malformed JSON output: {error}"))
}

fn browser_dynamic_checks_from_payload(payload: &Value) -> Vec<BrowserVerifierCheck> {
    payload
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|check| {
            let name = check.get("name")?.as_str()?.to_owned();
            let detail = check
                .get("detail")
                .and_then(Value::as_str)
                .or_else(|| check.get("message").and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned();
            let status = check
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(match status {
                "passed" | "pass" | "ok" | "completed" => BrowserVerifierCheck::pass(name, detail),
                "blocked" | "partial" | "skipped" => BrowserVerifierCheck::blocked(name, detail),
                _ => BrowserVerifierCheck::fail(name, detail),
            })
        })
        .collect()
}

pub(crate) const BROWSER_DYNAMIC_PLAYWRIGHT_SCRIPT: &str = r##"
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";

const [, , entryPath, nodeModules, checksJson = "[]"] = process.argv;
const requested = new Set(JSON.parse(checksJson));
const require = createRequire(import.meta.url);
const { chromium } = require(path.join(nodeModules, "playwright"));
const checks = [];
const consoleMessages = [];
let browser;
let browserName = "unknown";

function digest(value) {
  return createHash("sha256").update(String(value ?? "")).digest("hex");
}

function record(name, passed, detail, extra = {}) {
  const item = { name, status: passed ? "passed" : "failed", detail, ...extra };
  checks.push(item);
  if (!passed) {
    const error = new Error(`${name}: ${detail}`);
    error.check = item;
    throw error;
  }
}

function wants(name) {
  return requested.has(name);
}

function appendConsoleHealthCheck() {
  const existing = checks.find((check) => check.name === "browser_dynamic.console_errors");
  if (existing) return existing;
  const errors = consoleMessages.filter((message) => message.toLowerCase().includes("error"));
  const item = {
    name: "browser_dynamic.console_errors",
    status: errors.length === 0 ? "passed" : "failed",
    detail: errors.length === 0 ? "no browser console errors" : `browser errors: ${errors.slice(0, 3).join(" | ")}`,
    consoleMessages,
  };
  checks.push(item);
  return item;
}

function assertConsoleHealth() {
  const item = appendConsoleHealthCheck();
  if (item.status === "failed") {
    const error = new Error(`${item.name}: ${item.detail}`);
    error.check = item;
    throw error;
  }
}

async function launchBrowser() {
  const attempts = [
    ["bundled", {}],
    ["msedge", { channel: "msedge" }],
    ["chrome", { channel: "chrome" }],
  ];
  const errors = [];
  for (const [name, options] of attempts) {
    try {
      browser = await chromium.launch({ headless: true, ...options });
      browserName = name;
      return;
    } catch (error) {
      errors.push(`${name}: ${error.message}`);
    }
  }
  record("browser_dynamic.launch", false, "could not launch Chromium, Edge, or Chrome", { errors });
}

async function readState(page) {
  return page.evaluate(() => {
    const raw = window.__snakeTestState;
    let value = null;
    if (typeof raw === "function") {
      value = raw();
    } else if (raw && typeof raw.snapshot === "function") {
      value = raw.snapshot();
    } else if (raw && typeof raw === "object") {
      value = raw;
    }
    return value ? JSON.parse(JSON.stringify(value)) : null;
  }).catch(() => null);
}

function stateGameOver(state) {
  if (!state) return false;
  const status = String(state.status ?? state.state ?? "").toLowerCase();
  return state.gameOver === true || status === "game_over" || status === "game over" || status === "ended";
}

function headOf(state) {
  if (!state) return null;
  return state.head ?? state.snakeHead ?? (Array.isArray(state.snake) ? state.snake[0] : null);
}

function directionOf(state) {
  if (!state) return null;
  return state.direction ?? state.dir ?? state.velocity ?? null;
}

function numberValue(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function progressed(before, after) {
  if (!before || !after) return false;
  const beforeTick = numberValue(before.tick ?? before.ticks ?? before.frame);
  const afterTick = numberValue(after.tick ?? after.ticks ?? after.frame);
  if (beforeTick !== null && afterTick !== null && afterTick > beforeTick) return true;
  const beforeHead = headOf(before);
  const afterHead = headOf(after);
  return Boolean(beforeHead && afterHead && (beforeHead.x !== afterHead.x || beforeHead.y !== afterHead.y));
}

async function canvasDigest(page) {
  const count = await page.locator("canvas").count();
  if (count === 0) return null;
  return page.$eval("canvas", (canvas) => canvas.toDataURL("image/png")).then(digest).catch(() => null);
}

async function snapshot(page) {
  const [canvas, text, state] = await Promise.all([
    canvasDigest(page),
    page.locator("body").innerText({ timeout: 2000 }).catch(() => ""),
    readState(page),
  ]);
  return {
    canvas,
    textDigest: digest(text),
    state,
    stateDigest: digest(JSON.stringify(state ?? null)),
  };
}

function snapshotChanged(before, after) {
  return Boolean(
    before.canvas && after.canvas && before.canvas !== after.canvas ||
    before.textDigest !== after.textDigest ||
    before.stateDigest !== after.stateDigest ||
    progressed(before.state, after.state)
  );
}

async function assertNotGameOver(page, label) {
  const state = await readState(page);
  record(`snake_gameplay_browser.no_game_over.${label}`, !stateGameOver(state), `no Game Over during ${label}`, { state });
  return state;
}

async function waitForProgress(page, label, before) {
  let last = before;
  for (let attempt = 0; attempt < 10; attempt++) {
    await page.waitForTimeout(150);
    await assertNotGameOver(page, label);
    const current = await snapshot(page);
    if (snapshotChanged(before, current) && snapshotChanged(last, current)) {
      return current;
    }
    last = current;
  }
  record(`snake_gameplay_browser.progress.${label}`, false, `game did not visibly progress during ${label}`, { before, last });
}

function visibleScoreIsZero(scoreText) {
  const matches = String(scoreText ?? "").match(/-?\d+/g);
  if (!matches || matches.length === 0) return true;
  return Number(matches[matches.length - 1]) === 0;
}

async function runBasicBrowserChecks(page) {
  await page.goto(pathToFileURL(entryPath).href, { waitUntil: "load", timeout: 15000 });
  record("browser_dynamic.page_opened", true, `opened page with ${browserName}`);
  const bodyBox = await page.locator("body").boundingBox().catch(() => null);
  const canvasCount = await page.locator("canvas").count();
  const bodyText = await page.locator("body").innerText({ timeout: 2000 }).catch(() => "");
  record(
    "browser_dynamic.visible_surface",
    Boolean(bodyBox && bodyBox.width > 0 && bodyBox.height > 0 && (canvasCount > 0 || bodyText.trim().length > 0)),
    `visible body with ${canvasCount} canvas element(s)`
  );
}

async function runGameplayChecks(page) {
  let starter = page.locator("#start-btn, #play-btn, #startOverlay, #start-overlay, #startScreen, #start-screen, .start-overlay, .start-screen, [data-action='start'], [data-testid='start-button']").first();
  if (!(await starter.isVisible().catch(() => false))) {
    starter = page.getByRole("button", { name: /start|play game|begin/i }).first();
  }
  if (await starter.isVisible().catch(() => false)) {
    await starter.click({ timeout: 1500 }).catch(() => {});
    await page.waitForTimeout(250);
  }
  const before = await snapshot(page);
  await page.keyboard.press("ArrowRight").catch(() => {});
  await page.keyboard.press("Space").catch(() => {});
  const control = page.locator("[data-plant], .plant-card, button, [role='button']").first();
  if (await control.count().catch(() => 0) > 0) {
    await control.click({ timeout: 1500 }).catch(() => {});
  }
  const canvas = page.locator("canvas").first();
  const box = await canvas.boundingBox().catch(() => null);
  if (box) {
    await page.mouse.click(box.x + box.width * 0.35, box.y + box.height * 0.45).catch(() => {});
    await page.mouse.click(box.x + box.width * 0.65, box.y + box.height * 0.55).catch(() => {});
  } else {
    await page.mouse.click(300, 300).catch(() => {});
  }
  await page.waitForTimeout(1800);
  const after = await snapshot(page);
  const changed = snapshotChanged(before, after);
  record("gameplay_browser.interaction_probe", true, "start, keyboard, and pointer probes were dispatched");
  assertConsoleHealth();
  record(
    "gameplay_browser.progress",
    changed,
    changed
      ? "visual, DOM, or exposed state changed after time/input"
      : "expected visual, DOM, or exposed state to change after time/input, but no progress was observed",
    { before, after }
  );
}

async function runSnakeChecks(page) {
  const state = await readState(page);
  record("snake_gameplay_browser.test_state", Boolean(state), "window.__snakeTestState is readable");
  await assertNotGameOver(page, "initial_load");
  await page.waitForTimeout(350);
  await assertNotGameOver(page, "post_load_idle");
  const beforeRight = await snapshot(page);
  await page.keyboard.press("ArrowRight");
  const afterRight = await waitForProgress(page, "arrow_right", beforeRight);
  await page.keyboard.press("ArrowLeft");
  await page.waitForTimeout(220);
  const afterReverse = await assertNotGameOver(page, "opposite_direction_guard");
  const reverseDirection = directionOf(afterReverse);
  record(
    "snake_gameplay_browser.opposite_reversal_prevented",
    !(reverseDirection && Number(reverseDirection.x) < 0),
    "opposite direction did not immediately reverse into left movement",
    { state: afterReverse }
  );
  await page.keyboard.press("ArrowDown");
  await waitForProgress(page, "arrow_down", afterRight);
  const restart = page.locator("#restart-btn, [data-testid='restart-button']").first();
  record("snake_gameplay_browser.restart_control", await restart.count() > 0, "restart control exists");
  await restart.click({ timeout: 5000 });
  await page.waitForTimeout(250);
  const afterRestart = await assertNotGameOver(page, "restart");
  const scoreText = await page.locator("#score").first().textContent({ timeout: 1000 }).catch(() => "");
  record("snake_gameplay_browser.restart_score", visibleScoreIsZero(scoreText), "restart reset the visible score", { scoreText });
  const restartSnapshot = await snapshot(page);
  await page.keyboard.press("ArrowRight");
  await waitForProgress(page, "post_restart", { ...restartSnapshot, state: afterRestart });
}

try {
  await launchBrowser();
  const page = await browser.newPage({ viewport: { width: 900, height: 700 } });
  page.on("console", (message) => {
    if (["error", "warning"].includes(message.type())) {
      consoleMessages.push(`${message.type()}: ${message.text()}`);
    }
  });
  page.on("pageerror", (error) => {
    const stack = String(error.stack ?? error.message).split("\n").slice(0, 3).join(" ");
    consoleMessages.push(`pageerror: ${stack}`);
  });

  await runBasicBrowserChecks(page);
  if (wants("gameplay_browser")) {
    await runGameplayChecks(page);
  }
  if (wants("snake_gameplay_browser")) {
    await runSnakeChecks(page);
  }
  assertConsoleHealth();

  console.log(JSON.stringify({
    status: "ok",
    browser: browserName,
    checks,
    console_messages: consoleMessages,
  }));
} catch (error) {
  appendConsoleHealthCheck();
  console.log(JSON.stringify({
    status: "failed",
    reason: error.message,
    browser: browserName,
    checks,
    console_messages: consoleMessages,
  }));
  process.exitCode = 1;
} finally {
  if (browser) {
    await browser.close();
  }
}
"##;
