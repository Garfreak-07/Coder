use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use coder_core::FinalReport;
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunRequest, HarnessRunResult,
};
use coder_store::RunStore;
use serde_json::{json, Value};

use crate::{
    browser_verifier_dynamic::{
        BrowserDynamicRunInput, BrowserDynamicRunner, ProcessBrowserDynamicRunner,
    },
    native_selected_tools,
    workflow_reports::string_array,
};

#[derive(Debug, Clone)]
pub(crate) struct BrowserVerifierCheck {
    pub(crate) name: String,
    pub(crate) status: BrowserVerifierCheckStatus,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserVerifierCheckStatus {
    Passed,
    Failed,
    Blocked,
}

impl BrowserVerifierCheckStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }
}

impl BrowserVerifierCheck {
    pub(crate) fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: BrowserVerifierCheckStatus::Passed,
            detail: detail.into(),
        }
    }

    pub(crate) fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: BrowserVerifierCheckStatus::Failed,
            detail: detail.into(),
        }
    }

    pub(crate) fn blocked(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: BrowserVerifierCheckStatus::Blocked,
            detail: detail.into(),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "status": self.status.as_str(),
            "detail": self.detail
        })
    }
}

#[derive(Clone)]
pub struct BrowserVerifierBackend {
    store: RunStore,
    dynamic_runner: Arc<dyn BrowserDynamicRunner>,
}

impl std::fmt::Debug for BrowserVerifierBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserVerifierBackend")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl Default for BrowserVerifierBackend {
    fn default() -> Self {
        Self::new(RunStore::new(browser_verifier_default_store_root()))
    }
}

impl BrowserVerifierBackend {
    pub fn new(store: RunStore) -> Self {
        Self {
            store,
            dynamic_runner: Arc::new(ProcessBrowserDynamicRunner),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_dynamic_runner(
        store: RunStore,
        dynamic_runner: Arc<dyn BrowserDynamicRunner>,
    ) -> Self {
        Self {
            store,
            dynamic_runner,
        }
    }
}

pub(crate) fn browser_verifier_default_store_root() -> PathBuf {
    if let Some(cache_root) = env::var_os("CODER_CACHE_DIR")
        .and_then(|value| value.into_string().ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(cache_root).join("browser-verifier-store");
    }

    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".coder")
        .join("tmp")
        .join("browser-verifier-default-store")
}

#[async_trait]
impl HarnessBackend for BrowserVerifierBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        let repo_root = if request.repo_root.trim().is_empty() {
            ".".to_owned()
        } else {
            request.repo_root.clone()
        };
        let selected_checks = browser_verifier_selected_checks(&request);
        let mut checks = Vec::new();
        let mut dynamic_evidence = Vec::new();
        if selected_checks.is_empty() {
            checks.push(BrowserVerifierCheck::pass(
                "browser_verifier.skipped",
                "task did not request browser or game verification",
            ));
        } else {
            if selected_checks.contains("browser_static") {
                checks.extend(run_browser_static_checks(&repo_root));
            }
            if selected_checks.contains("gameplay_static") {
                checks.extend(run_gameplay_static_checks(&repo_root));
            }
            if selected_checks.contains("snake_gameplay_static") {
                checks.extend(run_snake_gameplay_static_checks(&repo_root));
            }
            if browser_verifier_has_dynamic_checks(&selected_checks) {
                let dynamic = self
                    .dynamic_runner
                    .run(BrowserDynamicRunInput {
                        run_id: request.run_id.as_str().to_owned(),
                        repo_root: repo_root.clone(),
                        runtime_root: self
                            .store
                            .root()
                            .join("tmp")
                            .join("runtime-cache")
                            .join("browser-verifier"),
                        task: request.task.clone(),
                        selected_checks: selected_checks.iter().cloned().collect(),
                    })
                    .await;
                checks.extend(dynamic.checks);
                dynamic_evidence.push(dynamic.evidence);
            }
        }

        let failed_checks = checks
            .iter()
            .filter(|check| check.status == BrowserVerifierCheckStatus::Failed)
            .map(|check| format!("{}: {}", check.name, check.detail))
            .collect::<Vec<_>>();
        let blocked_checks = checks
            .iter()
            .filter(|check| check.status == BrowserVerifierCheckStatus::Blocked)
            .map(|check| format!("{}: {}", check.name, check.detail))
            .collect::<Vec<_>>();
        let status = if !failed_checks.is_empty() {
            "failed"
        } else if !blocked_checks.is_empty() {
            "blocked"
        } else {
            "completed"
        };
        let summary = if selected_checks.is_empty() {
            "browser verification skipped"
        } else if status == "completed" {
            "browser verification passed"
        } else if status == "blocked" {
            "browser verification blocked"
        } else {
            "browser verification failed"
        };
        let evidence_payload = json!({
            "source": "browser-verifier",
            "status": status,
            "summary": summary,
            "repo_root": repo_root,
            "task": request.task,
            "selected_checks": selected_checks.iter().cloned().collect::<Vec<_>>(),
            "checks": checks.iter().map(BrowserVerifierCheck::to_json).collect::<Vec<_>>(),
            "dynamic": dynamic_evidence
        });
        let evidence_text = serde_json::to_string_pretty(&evidence_payload)
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        let evidence_ref = self
            .store
            .write_large_text_ref(&evidence_text)
            .map_err(|error| HarnessError::Failed(error.to_string()))?;
        let evidence_reference = coder_core::EvidenceRef {
            kind: "browser_verification".to_owned(),
            reference: evidence_ref.blob_ref.clone(),
        };
        let check_summaries = checks
            .iter()
            .map(|check| {
                format!(
                    "browser-verifier: {} {} - {}",
                    check.name,
                    check.status.as_str(),
                    check.detail
                )
            })
            .collect::<Vec<_>>();
        let mut report = if status == "completed" {
            FinalReport::completed(summary)
        } else if status == "blocked" {
            FinalReport::blocked(summary, blocked_checks.join("; "))
        } else {
            FinalReport::failed(summary, failed_checks.join("; "))
        };
        report.checks = check_summaries;
        report.evidence_refs = vec![evidence_reference];

        let mut terminal_payload = json!({
            "status": status,
            "summary": summary,
            "source": "browser-verifier",
            "checks": checks.iter().map(BrowserVerifierCheck::to_json).collect::<Vec<_>>(),
            "evidence": {
                "preview": evidence_ref.preview,
                "truncated": evidence_ref.truncated,
                "blob_ref": evidence_ref.blob_ref
            }
        });
        let issue_checks = failed_checks
            .iter()
            .chain(blocked_checks.iter())
            .cloned()
            .collect::<Vec<_>>();
        if !issue_checks.is_empty() {
            terminal_payload["reason"] = json!(issue_checks.join("; "));
            terminal_payload["remaining_work"] = json!(failed_checks
                .iter()
                .chain(blocked_checks.iter())
                .map(|check| format!("Fix browser verification check: {check}"))
                .collect::<Vec<_>>());
        }
        let terminal_kind = if status == "completed" {
            "verification.completed"
        } else {
            "verification.failed"
        };
        let events = vec![
            HarnessRunEvent::new(
                "verification.started",
                json!({
                    "status": "started",
                    "summary": "browser verifier started",
                    "source": "browser-verifier",
                    "selected_checks": selected_checks.iter().cloned().collect::<Vec<_>>()
                }),
            ),
            HarnessRunEvent::new(terminal_kind, terminal_payload)
                .with_ref("browser_verification", evidence_ref.blob_ref),
            HarnessRunEvent::new(
                format!("backend.browser_verifier.{status}"),
                json!({
                    "backend": "browser-verifier",
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "status": status,
                    "check_count": checks.len(),
                    "failed_check_count": failed_checks.len()
                }),
            ),
        ];

        Ok(HarnessRunResult {
            status: status.to_owned(),
            report: Some(report),
            events,
        })
    }
}

pub(crate) fn browser_verifier_selected_checks(request: &HarnessRunRequest) -> BTreeSet<String> {
    let tools = native_selected_tools(request);
    let configured = string_array(
        request
            .backend_context
            .pointer("/coder/harness/verification/allowed_checks"),
    );
    let configured_has_auto =
        configured.is_empty() || configured.iter().any(|check| check == "auto");
    let configured_has_auto_static = configured_has_auto
        || configured
            .iter()
            .any(|check| check == "auto_browser_static");
    let configured_has_auto_dynamic = configured_has_auto
        || configured
            .iter()
            .any(|check| check == "auto_browser_dynamic");
    let mut selected = BTreeSet::new();
    for check in configured {
        if check.trim().starts_with("auto") {
            continue;
        }
        let normalized = browser_verifier_check_name(&check);
        if normalized.starts_with("auto") {
            continue;
        }
        if browser_verifier_tool_enabled(&tools, normalized) {
            selected.insert(normalized.to_owned());
        }
    }
    if configured_has_auto_static || configured_has_auto_dynamic {
        let task = browser_verifier_task_text(request);
        if configured_has_auto_static
            && browser_task_requested(&task)
            && browser_verifier_tool_enabled(&tools, "browser_static")
        {
            selected.insert("browser_static".to_owned());
        }
        if configured_has_auto_static
            && game_task_requested(&task)
            && browser_verifier_tool_enabled(&tools, "gameplay_static")
        {
            selected.insert("gameplay_static".to_owned());
        }
        if configured_has_auto_static
            && snake_task_requested(&task)
            && browser_verifier_tool_enabled(&tools, "snake_gameplay_static")
        {
            selected.insert("snake_gameplay_static".to_owned());
        }
        if configured_has_auto_dynamic
            && browser_task_requested(&task)
            && browser_verifier_tool_enabled(&tools, "browser_dynamic")
        {
            selected.insert("browser_dynamic".to_owned());
        }
        if configured_has_auto_dynamic
            && game_task_requested(&task)
            && browser_verifier_tool_enabled(&tools, "gameplay_browser")
        {
            selected.insert("gameplay_browser".to_owned());
        }
        if configured_has_auto_dynamic
            && snake_task_requested(&task)
            && browser_verifier_tool_enabled(&tools, "snake_gameplay_browser")
        {
            selected.insert("snake_gameplay_browser".to_owned());
        }
    }
    selected
}

fn browser_verifier_check_name(check: &str) -> &str {
    match check.trim() {
        "browser" | "browser_static" | "auto_browser_static" => "browser_static",
        "game" | "gameplay" | "gameplay_static" => "gameplay_static",
        "snake" | "snake_gameplay" | "snake_gameplay_static" => "snake_gameplay_static",
        "browser_dynamic" | "browser_live" | "browser_playwright" | "auto_browser_dynamic" => {
            "browser_dynamic"
        }
        "gameplay_browser" | "gameplay_dynamic" | "gameplay_playwright" => "gameplay_browser",
        "snake_gameplay_browser" | "snake_browser" | "snake_playwright" => "snake_gameplay_browser",
        "auto" => "auto",
        other => other,
    }
}

fn browser_verifier_tool_enabled(tools: &BTreeSet<String>, check: &str) -> bool {
    if tools.is_empty() {
        return true;
    }
    tools.contains(check)
        || match check {
            "browser_static" => tools.contains("browser"),
            "gameplay_static" => tools.contains("gameplay"),
            "snake_gameplay_static" => tools.contains("snake_gameplay"),
            "browser_dynamic" => tools.contains("browser") || tools.contains("browser_playwright"),
            "gameplay_browser" => tools.contains("gameplay") || tools.contains("gameplay_dynamic"),
            "snake_gameplay_browser" => {
                tools.contains("snake_gameplay") || tools.contains("snake_gameplay_dynamic")
            }
            _ => false,
        }
}

pub(crate) fn browser_verifier_has_dynamic_checks(checks: &BTreeSet<String>) -> bool {
    checks
        .iter()
        .any(|check| browser_verifier_is_dynamic_check(check))
}

pub(crate) fn browser_verifier_is_dynamic_check(check: &str) -> bool {
    matches!(
        check,
        "browser_dynamic" | "gameplay_browser" | "snake_gameplay_browser"
    )
}

fn browser_verifier_task_text(request: &HarnessRunRequest) -> String {
    let mut text = request.task.clone();
    if let Some(plan) = request.backend_context.pointer("/coder/plan_context") {
        text.push('\n');
        text.push_str(&plan.to_string());
    }
    text.to_ascii_lowercase()
}

fn browser_task_requested(task: &str) -> bool {
    contains_any_ascii_word(
        task,
        &[
            "browser", "web", "frontend", "html", "css", "canvas", "dom", "page", "site",
            "website", "webpage",
        ],
    ) || task.contains("index.html")
        || task.contains("\u{7f51}\u{9875}")
}

fn game_task_requested(task: &str) -> bool {
    contains_any_ascii_word(
        task,
        &[
            "game", "snake", "plant", "zombie", "pvz", "canvas", "score", "restart", "keyboard",
            "wasd", "arrow",
        ],
    ) || task.contains("\u{6e38}\u{620f}")
        || task.contains("\u{690d}\u{7269}")
        || task.contains("\u{50f5}\u{5c38}")
}

fn snake_task_requested(task: &str) -> bool {
    contains_any_ascii_word(task, &["snake"]) || task.contains("\u{8d2a}\u{5403}\u{86c7}")
}

fn contains_any_ascii_word(value: &str, words: &[&str]) -> bool {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| words.contains(&word))
}

pub(crate) fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

pub(crate) fn run_browser_static_checks(repo_root: &str) -> Vec<BrowserVerifierCheck> {
    let index = read_repo_text(repo_root, "index.html", 256_000);
    let script_text = read_browser_script_text(repo_root);
    let has_framework_entry = browser_any_file_exists(
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
    );
    let mut checks = Vec::new();
    checks.push(if index.is_ok() || has_framework_entry {
        BrowserVerifierCheck::pass("browser_static.entry", "browser entry file found")
    } else {
        BrowserVerifierCheck::fail(
            "browser_static.entry",
            "expected index.html or a common frontend entry file",
        )
    });
    checks.push(if script_text.trim().is_empty() {
        BrowserVerifierCheck::fail(
            "browser_static.script",
            "expected JavaScript or TypeScript entry code for browser behavior",
        )
    } else {
        BrowserVerifierCheck::pass("browser_static.script", "browser script code found")
    });
    if let Ok(index_text) = index {
        let lower = index_text.to_ascii_lowercase();
        checks.push(if lower.contains("<script") || lower.contains("main.js") {
            BrowserVerifierCheck::pass("browser_static.script_reference", "HTML loads script code")
        } else {
            BrowserVerifierCheck::fail(
                "browser_static.script_reference",
                "index.html does not include a script tag or main.js reference",
            )
        });
        checks.push(
            if lower.contains("<link") || lower.contains("<style") || css_file_exists(repo_root) {
                BrowserVerifierCheck::pass(
                    "browser_static.styles",
                    "HTML has styles or stylesheet file",
                )
            } else {
                BrowserVerifierCheck::fail(
                    "browser_static.styles",
                    "expected a stylesheet link, style tag, or CSS file",
                )
            },
        );
    }
    checks
}

pub(crate) fn run_gameplay_static_checks(repo_root: &str) -> Vec<BrowserVerifierCheck> {
    let script_text = read_browser_script_text(repo_root).to_ascii_lowercase();
    vec![
        if contains_any(
            &script_text,
            &[
                "keydown",
                "keyup",
                "pointerdown",
                "mousedown",
                "click",
                "touchstart",
            ],
        ) {
            BrowserVerifierCheck::pass("gameplay_static.input", "game input handler found")
        } else {
            BrowserVerifierCheck::fail(
                "gameplay_static.input",
                "expected keyboard, pointer, click, or touch input handling",
            )
        },
        if contains_any(
            &script_text,
            &["requestanimationframe", "setinterval", "settimeout"],
        ) {
            BrowserVerifierCheck::pass("gameplay_static.loop", "game update loop found")
        } else {
            BrowserVerifierCheck::fail(
                "gameplay_static.loop",
                "expected requestAnimationFrame, setInterval, or setTimeout loop",
            )
        },
        if contains_any(
            &script_text,
            &[
                "getcontext",
                "fillrect",
                "drawimage",
                "queryselector",
                "getelementbyid",
                "classlist",
            ],
        ) {
            BrowserVerifierCheck::pass(
                "gameplay_static.rendering",
                "rendering surface update found",
            )
        } else {
            BrowserVerifierCheck::fail(
                "gameplay_static.rendering",
                "expected canvas or DOM rendering updates",
            )
        },
    ]
}

pub(crate) fn run_snake_gameplay_static_checks(repo_root: &str) -> Vec<BrowserVerifierCheck> {
    let index_text = read_repo_text(repo_root, "index.html", 256_000)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let script_text = read_browser_script_text(repo_root).to_ascii_lowercase();
    let combined = format!("{index_text}\n{script_text}");
    vec![
        if contains_any(&combined, &["restart-btn", "restart-button", "restart"]) {
            BrowserVerifierCheck::pass("snake_gameplay_static.restart", "restart control found")
        } else {
            BrowserVerifierCheck::fail(
                "snake_gameplay_static.restart",
                "expected restart button or restart control",
            )
        },
        if script_text.contains("__snaketeststate") {
            BrowserVerifierCheck::pass(
                "snake_gameplay_static.test_state",
                "read-only window.__snakeTestState hook found",
            )
        } else {
            BrowserVerifierCheck::fail(
                "snake_gameplay_static.test_state",
                "expected read-only window.__snakeTestState hook for browser validation",
            )
        },
        if contains_any(&script_text, &["gameover", "game_over", "score"]) {
            BrowserVerifierCheck::pass("snake_gameplay_static.state", "gameOver/score state found")
        } else {
            BrowserVerifierCheck::fail(
                "snake_gameplay_static.state",
                "expected gameOver and score state in the snake implementation",
            )
        },
    ]
}

fn read_browser_script_text(repo_root: &str) -> String {
    let mut sources = [
        "main.js",
        "script.js",
        "game.js",
        "src/main.js",
        "src/main.jsx",
        "src/main.tsx",
        "src/App.jsx",
        "src/App.tsx",
        "app/page.tsx",
        "pages/index.tsx",
    ]
    .iter()
    .filter_map(|path| read_repo_text(repo_root, path, 512_000).ok())
    .collect::<Vec<_>>();
    if let Ok(index) = read_repo_text(repo_root, "index.html", 512_000) {
        if index.to_ascii_lowercase().contains("<script") {
            sources.push(index);
        }
    }
    sources.join("\n")
}

fn read_repo_text(repo_root: &str, relative: &str, max_chars: usize) -> Result<String, String> {
    let path = Path::new(repo_root).join(relative);
    let metadata = fs::metadata(&path).map_err(|error| format!("{}: {}", relative, error))?;
    if !metadata.is_file() {
        return Err(format!("{relative} is not a file"));
    }
    let text = fs::read_to_string(&path).map_err(|error| format!("{}: {}", relative, error))?;
    if text.chars().count() <= max_chars {
        Ok(text)
    } else {
        Ok(text.chars().take(max_chars).collect())
    }
}

pub(crate) fn browser_any_file_exists(repo_root: &str, relatives: &[&str]) -> bool {
    relatives
        .iter()
        .any(|relative| Path::new(repo_root).join(relative).is_file())
}

fn css_file_exists(repo_root: &str) -> bool {
    browser_any_file_exists(
        repo_root,
        &[
            "style.css",
            "styles.css",
            "src/style.css",
            "src/styles.css",
            "src/App.css",
            "app/globals.css",
        ],
    )
}
