use std::{collections::BTreeSet, fs, net::SocketAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand};
use coder_config::{
    load_project_config, validate_project_config, ProjectConfig, ValidationIssue, ValidationLevel,
};
use coder_core::RunId;
#[cfg(test)]
use coder_core::{RunState, RunStatus, WorkflowId};
use coder_events::CoderEvent;
use coder_server::{run_embedded_workflow, serve, ApiState};
use coder_store::{RepoEvidenceKind, RepoEvidenceRef, RunStore};
use coder_tools::{
    apply_patch_file, find_files, git_diff, git_status, preview_patch_file, read_file,
    read_file_range, run_command, search_text, CommandRunEvidence, CommandRunRequest,
    PatchApplyEvidence, PatchApplyRequest, PatchPreviewEvidence, RepoToolConfig,
};
use coder_workflow::{MockWorkflowRunner, WorkflowRunOptions};
use serde_json::json;

const DEFAULT_STORE: &str = ".coder";

#[derive(Debug, Parser)]
#[command(name = "coder-rust")]
#[command(about = "Coder control-plane runtime")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Doctor,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },
    Tools {
        #[command(subcommand)]
        command: ToolsCommand,
    },
    Server {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8766)]
        port: u16,
        #[arg(long, default_value = DEFAULT_STORE)]
        store: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Validate {
        #[arg(long, default_value = "examples/coder.yaml")]
        path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    Validate {
        #[arg(long, default_value = "examples/coder.yaml")]
        config: PathBuf,
    },
    Preview {
        #[arg(long, default_value = "examples/coder.yaml")]
        config: PathBuf,
        workflow_id: String,
        task: String,
    },
    Run {
        #[arg(long)]
        mock: bool,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value = "examples/coder.yaml")]
        config: PathBuf,
        #[arg(long, default_value = DEFAULT_STORE)]
        store: PathBuf,
        workflow_id: String,
        task: String,
    },
}

#[derive(Debug, Subcommand)]
enum RunsCommand {
    List {
        #[arg(long, default_value = DEFAULT_STORE)]
        store: PathBuf,
    },
    Show {
        #[arg(long, default_value = DEFAULT_STORE)]
        store: PathBuf,
        run_id: String,
    },
    Evidence {
        #[arg(long, default_value = DEFAULT_STORE)]
        store: PathBuf,
        run_id: String,
    },
    Report {
        #[arg(long, default_value = DEFAULT_STORE)]
        store: PathBuf,
        #[arg(long, default_value_t = false)]
        write: bool,
        run_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum ToolsCommand {
    FindFiles {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        query: Option<String>,
        #[arg(long = "extension")]
        extensions: Vec<String>,
        #[arg(long, default_value_t = coder_tools::DEFAULT_MAX_FILE_RESULTS)]
        max_results: usize,
        #[command(flatten)]
        evidence: EvidenceRecordArgs,
    },
    ReadFile {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = coder_tools::DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
        path: PathBuf,
        #[command(flatten)]
        evidence: EvidenceRecordArgs,
    },
    ReadFileRange {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = 1)]
        start_line: usize,
        #[arg(long, default_value_t = 120)]
        max_lines: usize,
        #[arg(long, default_value_t = 16_000)]
        max_chars: usize,
        path: PathBuf,
        #[command(flatten)]
        evidence: EvidenceRecordArgs,
    },
    SearchText {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = coder_tools::DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
        #[arg(long, default_value_t = coder_tools::DEFAULT_MAX_SEARCH_MATCHES)]
        max_matches: usize,
        query: String,
        #[command(flatten)]
        evidence: EvidenceRecordArgs,
    },
    GitStatus {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    GitDiff {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = coder_tools::DEFAULT_MAX_GIT_OUTPUT_BYTES)]
        max_output_bytes: usize,
        #[command(flatten)]
        evidence: EvidenceRecordArgs,
    },
    PatchPreview {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = coder_tools::DEFAULT_MAX_PATCH_BYTES)]
        max_patch_bytes: usize,
        patch_file: PathBuf,
        #[command(flatten)]
        evidence: EvidenceRecordArgs,
    },
    PatchApply {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = coder_tools::DEFAULT_MAX_PATCH_BYTES)]
        max_patch_bytes: usize,
        #[arg(long, default_value = "model")]
        source: String,
        #[arg(long, default_value_t = false)]
        approved: bool,
        patch_file: PathBuf,
        #[command(flatten)]
        evidence: EvidenceRecordArgs,
    },
    RunCommand {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        #[arg(long, default_value_t = coder_tools::DEFAULT_COMMAND_TIMEOUT_SECONDS)]
        timeout_seconds: u64,
        #[arg(long, default_value_t = coder_tools::DEFAULT_MAX_COMMAND_OUTPUT_BYTES)]
        max_output_bytes: usize,
        #[arg(long, default_value = "model")]
        source: String,
        #[arg(long, default_value_t = false)]
        sandbox: bool,
        #[arg(long, default_value_t = false)]
        approved: bool,
        #[arg(required = true, trailing_var_arg = true, num_args = 1..)]
        argv: Vec<String>,
        #[command(flatten)]
        evidence: EvidenceRecordArgs,
    },
}

#[derive(Debug, Clone, Args)]
struct EvidenceRecordArgs {
    #[arg(long)]
    store: Option<PathBuf>,
    #[arg(long)]
    run_id: Option<String>,
}

fn ensure_valid_config(config: &ProjectConfig) -> anyhow::Result<()> {
    let report = validate_project_config(config);
    if !report.is_pass() {
        anyhow::bail!("invalid config: {}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

fn workflow_preview_json(
    config: &ProjectConfig,
    workflow_id: &str,
    task: &str,
) -> serde_json::Value {
    let mut issues = validate_project_config(config).issues;
    let workflow = config.workflows.get(workflow_id);
    if workflow.is_none() {
        issues.push(validation_issue(
            ValidationLevel::Error,
            "workflow_not_found",
            format!("workflow '{workflow_id}' was not found"),
            "workflow_id",
        ));
    }
    if task.trim().is_empty() {
        issues.push(validation_issue(
            ValidationLevel::Error,
            "task_empty",
            "task must not be empty",
            "task",
        ));
    }
    let status = if issues
        .iter()
        .any(|issue| issue.level == ValidationLevel::Error)
    {
        "blocked"
    } else {
        "ready"
    };
    let backends = workflow
        .map(|workflow| {
            workflow
                .nodes
                .iter()
                .filter_map(|node| config.harnesses.get(&node.harness))
                .map(|harness| harness.backend.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    json!({
        "status": status,
        "requires_confirmation": status == "ready",
        "workflow_id": workflow_id,
        "task": task,
        "backends": backends,
        "issues": issues,
    })
}

fn validation_issue(
    level: ValidationLevel,
    code: impl Into<String>,
    message: impl Into<String>,
    target: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        level,
        code: code.into(),
        message: message.into(),
        target: target.into(),
    }
}

fn run_list_json(store: &RunStore) -> anyhow::Result<serde_json::Value> {
    Ok(json!({
        "runs": store.list_run_summaries()?,
    }))
}

fn run_detail_json(store: &RunStore, run_id: &RunId) -> anyhow::Result<serde_json::Value> {
    let metadata = store.read_metadata(run_id)?;
    let events = store.read_events(run_id)?;
    let report = store.read_report(run_id)?;
    let repo_evidence_count = store.repo_evidence_count(run_id)?;
    if metadata.is_none() && events.is_empty() && report.is_none() && repo_evidence_count == 0 {
        anyhow::bail!("run '{}' was not found", run_id.as_str());
    }
    Ok(json!({
        "run_id": run_id.as_str(),
        "metadata": metadata,
        "events": events,
        "report": report,
        "repo_evidence_count": repo_evidence_count,
    }))
}

fn run_repo_evidence_json(store: &RunStore, run_id: &RunId) -> anyhow::Result<serde_json::Value> {
    Ok(json!({
        "run_id": run_id.as_str(),
        "evidence": store.list_repo_evidence(run_id)?,
    }))
}

fn run_report_json(
    store: &RunStore,
    run_id: &RunId,
    write: bool,
) -> anyhow::Result<serde_json::Value> {
    let report = store.build_evidence_report(run_id)?;
    let report_ref = if write {
        Some(store.write_report(run_id, &report)?)
    } else {
        None
    };
    Ok(json!({
        "run_id": run_id.as_str(),
        "report_ref": report_ref,
        "report": report,
    }))
}

fn write_optional_repo_evidence(
    args: &EvidenceRecordArgs,
    kind: RepoEvidenceKind,
    repo: &std::path::Path,
    summary: impl Into<String>,
    payload: serde_json::Value,
) -> anyhow::Result<Option<RepoEvidenceRef>> {
    match (&args.store, &args.run_id) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => {
            anyhow::bail!("use --store and --run-id together when recording repo evidence");
        }
        (Some(store), Some(run_id)) => {
            let repo_root = fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
            let reference = RunStore::new(store.clone()).write_repo_evidence(
                &RunId::from_string(run_id.clone()),
                kind,
                repo_root.display().to_string(),
                Vec::new(),
                summary,
                payload,
            )?;
            Ok(Some(reference))
        }
    }
}

fn print_tool_output(
    output: serde_json::Value,
    evidence_ref: Option<RepoEvidenceRef>,
) -> anyhow::Result<()> {
    let response = if let Some(evidence_ref) = evidence_ref {
        json!({
            "evidence_ref": evidence_ref,
            "payload": output,
        })
    } else {
        output
    };
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn record_command_events(
    store: &RunStore,
    run_id: &RunId,
    output: &CommandRunEvidence,
    evidence_ref: &RepoEvidenceRef,
) -> anyhow::Result<()> {
    let mut sequence = store.read_events(run_id)?.len() as u64 + 1;
    let evidence_uri = format!("repo-evidence://{}", evidence_ref.ref_id);
    if output.blocked && output.requires_approval {
        store.append_event(
            run_id,
            &CoderEvent::new(
                run_id.clone(),
                sequence,
                "approval.requested",
                json!({
                    "approval_type": "command",
                    "approval_key": &output.approval_key,
                    "command": &output.command,
                    "cwd": &output.cwd,
                    "policy": &output.policy,
                    "evidence_ref": &evidence_ref.ref_id,
                }),
            )
            .with_ref("command_evidence", evidence_uri),
        )?;
        return Ok(());
    }

    store.append_event(
        run_id,
        &CoderEvent::new(
            run_id.clone(),
            sequence,
            "command.started",
            json!({
                "command": &output.command,
                "argv": &output.argv,
                "cwd": &output.cwd,
                "approval_key": &output.approval_key,
                "policy": &output.policy,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("command_evidence", evidence_uri.clone()),
    )?;
    sequence += 1;
    let kind = match output.status.as_str() {
        "completed" => "command.completed",
        "timeout" => "command.failed",
        _ => "command.failed",
    };
    store.append_event(
        run_id,
        &CoderEvent::new(
            run_id.clone(),
            sequence,
            kind,
            json!({
                "command": &output.command,
                "cwd": &output.cwd,
                "status": &output.status,
                "passed": output.passed,
                "returncode": output.returncode,
                "timed_out": output.timed_out,
                "output_preview": &output.output,
                "output_truncated": output.output_truncated,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("command_evidence", evidence_uri),
    )?;
    Ok(())
}

fn record_patch_preview_event(
    store: &RunStore,
    run_id: &RunId,
    patch_file: &str,
    output: &PatchPreviewEvidence,
    evidence_ref: &RepoEvidenceRef,
) -> anyhow::Result<()> {
    let sequence = store.read_events(run_id)?.len() as u64 + 1;
    let evidence_uri = format!("repo-evidence://{}", evidence_ref.ref_id);
    store.append_event(
        run_id,
        &CoderEvent::new(
            run_id.clone(),
            sequence,
            "patch.previewed",
            json!({
                "patch_file": patch_file,
                "file_count": output.file_count,
                "hunk_count": output.hunk_count,
                "additions": output.additions,
                "deletions": output.deletions,
                "truncated": output.truncated,
                "files": &output.files,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("patch_evidence", evidence_uri),
    )?;
    Ok(())
}

fn record_patch_apply_event(
    store: &RunStore,
    run_id: &RunId,
    output: &PatchApplyEvidence,
    evidence_ref: &RepoEvidenceRef,
) -> anyhow::Result<()> {
    let sequence = store.read_events(run_id)?.len() as u64 + 1;
    let evidence_uri = format!("repo-evidence://{}", evidence_ref.ref_id);
    if output.requires_approval {
        store.append_event(
            run_id,
            &CoderEvent::new(
                run_id.clone(),
                sequence,
                "approval.requested",
                json!({
                    "approval_type": "patch_apply",
                    "approval_key": &output.approval_key,
                    "patch_file": &output.patch_file,
                    "reason": &output.reason,
                    "files": &output.preview.files,
                    "evidence_ref": &evidence_ref.ref_id,
                }),
            )
            .with_ref("patch_evidence", evidence_uri),
        )?;
        return Ok(());
    }

    let kind = if output.applied {
        "patch.applied"
    } else {
        "patch.failed"
    };
    store.append_event(
        run_id,
        &CoderEvent::new(
            run_id.clone(),
            sequence,
            kind,
            json!({
                "status": &output.status,
                "patch_file": &output.patch_file,
                "applied": output.applied,
                "reason": &output.reason,
                "approval_key": &output.approval_key,
                "file_count": output.preview.file_count,
                "files": &output.preview.files,
                "evidence_ref": &evidence_ref.ref_id,
            }),
        )
        .with_ref("patch_evidence", evidence_uri),
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => {
            println!("coder: ok");
            println!("control_plane: rust_api_v3");
        }
        Command::Config {
            command: ConfigCommand::Validate { path },
        } => {
            let config = load_project_config(&path)?;
            let report = validate_project_config(&config);
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.is_pass() {
                std::process::exit(1);
            }
        }
        Command::Workflow {
            command: WorkflowCommand::Validate { config },
        } => {
            let config = load_project_config(&config)?;
            let report = validate_project_config(&config);
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.is_pass() {
                std::process::exit(1);
            }
        }
        Command::Workflow {
            command:
                WorkflowCommand::Preview {
                    config,
                    workflow_id,
                    task,
                },
        } => {
            let config = load_project_config(&config)?;
            let output = workflow_preview_json(&config, &workflow_id, &task);
            println!("{}", serde_json::to_string_pretty(&output)?);
            if output["status"] == "blocked" {
                std::process::exit(1);
            }
        }
        Command::Workflow {
            command:
                WorkflowCommand::Run {
                    mock,
                    repo,
                    config,
                    store,
                    workflow_id,
                    task,
                },
        } => {
            let config = load_project_config(&config)?;
            if mock {
                let runner = MockWorkflowRunner::new(&config, RunStore::new(store));
                let output = runner.run(&workflow_id, &task)?;
                println!("run_id={}", output.run_id);
                println!("report_ref={}", output.report_ref);
                println!("summary={}", output.report.summary);
            } else {
                ensure_valid_config(&config)?;
                let store = RunStore::new(store);
                let mut options = WorkflowRunOptions::new(workflow_id, task);
                options.repo_root = repo;
                let output = run_embedded_workflow(config, store, options).await?;
                println!("run_id={}", output.run_id);
                println!("report_ref={}", output.report_ref);
                println!("summary={}", output.report.summary);
            }
        }
        Command::Runs {
            command: RunsCommand::List { store },
        } => {
            let output = run_list_json(&RunStore::new(store))?;
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        Command::Runs {
            command: RunsCommand::Show { store, run_id },
        } => {
            let output = run_detail_json(&RunStore::new(store), &RunId::from_string(run_id))?;
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        Command::Runs {
            command: RunsCommand::Evidence { store, run_id },
        } => {
            let output =
                run_repo_evidence_json(&RunStore::new(store), &RunId::from_string(run_id))?;
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        Command::Runs {
            command:
                RunsCommand::Report {
                    store,
                    write,
                    run_id,
                },
        } => {
            let output =
                run_report_json(&RunStore::new(store), &RunId::from_string(run_id), write)?;
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        Command::Tools {
            command:
                ToolsCommand::FindFiles {
                    repo,
                    query,
                    extensions,
                    max_results,
                    evidence,
                },
        } => {
            let output = find_files(&repo, query.as_deref(), &extensions, max_results)?;
            let output_json = serde_json::to_value(&output)?;
            let payload = json!({
                "evidence_kind": "repo_evidence",
                "operation": "find_files",
                "query": query,
                "extensions": extensions,
                "max_results": max_results,
                "files": output_json,
            });
            let evidence_ref = write_optional_repo_evidence(
                &evidence,
                RepoEvidenceKind::RepoFileList,
                &repo,
                format!("Found {} repo file(s).", output.len()),
                payload,
            )?;
            print_tool_output(serde_json::to_value(&output)?, evidence_ref)?;
        }
        Command::Tools {
            command:
                ToolsCommand::ReadFile {
                    repo,
                    max_file_bytes,
                    path,
                    evidence,
                },
        } => {
            let requested_path = path.display().to_string();
            let output = read_file(
                &repo,
                path,
                &RepoToolConfig {
                    max_file_bytes,
                    max_search_matches: coder_tools::DEFAULT_MAX_SEARCH_MATCHES,
                },
            )?;
            let payload = json!({
                "evidence_kind": "repo_evidence",
                "operation": "read_file",
                "path": requested_path,
                "file": {
                    "path": output.path,
                    "size_bytes": output.size_bytes,
                    "content_chars": output.content.chars().count(),
                    "content_stored": false,
                    "content_note": "full file content is omitted from stored read_file evidence; use read_file_range for bounded content evidence",
                    "evidence_kind": output.evidence_kind
                },
            });
            let evidence_ref = write_optional_repo_evidence(
                &evidence,
                RepoEvidenceKind::RepoRead,
                &repo,
                format!("Read {}.", output.path),
                payload,
            )?;
            print_tool_output(serde_json::to_value(&output)?, evidence_ref)?;
        }
        Command::Tools {
            command:
                ToolsCommand::ReadFileRange {
                    repo,
                    start_line,
                    max_lines,
                    max_chars,
                    path,
                    evidence,
                },
        } => {
            let requested_path = path.display().to_string();
            let output = read_file_range(&repo, path, start_line, max_lines, max_chars)?;
            let output_json = serde_json::to_value(&output)?;
            let payload = json!({
                "evidence_kind": "repo_evidence",
                "operation": "read_file_range",
                "path": requested_path,
                "snippet": output_json,
            });
            let evidence_ref = write_optional_repo_evidence(
                &evidence,
                RepoEvidenceKind::RepoRead,
                &repo,
                format!(
                    "Read {}:{}-{}.",
                    output.path, output.start_line, output.end_line
                ),
                payload,
            )?;
            print_tool_output(serde_json::to_value(&output)?, evidence_ref)?;
        }
        Command::Tools {
            command:
                ToolsCommand::SearchText {
                    repo,
                    max_file_bytes,
                    max_matches,
                    query,
                    evidence,
                },
        } => {
            let output = search_text(
                &repo,
                &query,
                &RepoToolConfig {
                    max_file_bytes,
                    max_search_matches: max_matches,
                },
            )?;
            let output_json = serde_json::to_value(&output)?;
            let payload = json!({
                "evidence_kind": "repo_evidence",
                "operation": "search_text",
                "pattern": query,
                "max_results": max_matches,
                "hits": output_json,
            });
            let evidence_ref = write_optional_repo_evidence(
                &evidence,
                RepoEvidenceKind::RepoTextSearch,
                &repo,
                format!("Found {} repo text hit(s).", output.len()),
                payload,
            )?;
            print_tool_output(serde_json::to_value(&output)?, evidence_ref)?;
        }
        Command::Tools {
            command: ToolsCommand::GitStatus { repo },
        } => {
            let output = git_status(repo)?;
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        Command::Tools {
            command:
                ToolsCommand::GitDiff {
                    repo,
                    max_output_bytes,
                    evidence,
                },
        } => {
            let output = git_diff(&repo, max_output_bytes)?;
            let output_json = serde_json::to_value(&output)?;
            let payload = json!({
                "evidence_kind": "repo_evidence",
                "operation": "git_diff",
                "max_output_bytes": max_output_bytes,
                "diff": output_json,
            });
            let evidence_ref = write_optional_repo_evidence(
                &evidence,
                RepoEvidenceKind::RepoDiff,
                &repo,
                "Captured git diff preview.",
                payload,
            )?;
            print_tool_output(serde_json::to_value(&output)?, evidence_ref)?;
        }
        Command::Tools {
            command:
                ToolsCommand::PatchPreview {
                    repo,
                    max_patch_bytes,
                    patch_file,
                    evidence,
                },
        } => {
            let requested_patch_file = patch_file.display().to_string();
            let output = preview_patch_file(&repo, patch_file, max_patch_bytes)?;
            let output_json = serde_json::to_value(&output)?;
            let payload = json!({
                "evidence_kind": "repo_evidence",
                "operation": "patch_preview",
                "patch_file": requested_patch_file,
                "max_patch_bytes": max_patch_bytes,
                "preview": output_json,
            });
            let evidence_ref = write_optional_repo_evidence(
                &evidence,
                RepoEvidenceKind::RepoDiff,
                &repo,
                format!("Previewed patch touching {} file(s).", output.file_count),
                payload,
            )?;
            if let (Some(store), Some(run_id), Some(reference)) =
                (&evidence.store, &evidence.run_id, &evidence_ref)
            {
                record_patch_preview_event(
                    &RunStore::new(store.clone()),
                    &RunId::from_string(run_id.clone()),
                    &requested_patch_file,
                    &output,
                    reference,
                )?;
            }
            print_tool_output(serde_json::to_value(&output)?, evidence_ref)?;
        }
        Command::Tools {
            command:
                ToolsCommand::PatchApply {
                    repo,
                    max_patch_bytes,
                    source,
                    approved,
                    patch_file,
                    evidence,
                },
        } => {
            let output = apply_patch_file(
                &repo,
                PatchApplyRequest {
                    patch_file,
                    max_patch_bytes,
                    source,
                    approved,
                },
            )?;
            let output_json = serde_json::to_value(&output)?;
            let evidence_ref = write_optional_repo_evidence(
                &evidence,
                RepoEvidenceKind::RepoDiff,
                &repo,
                format!(
                    "Patch apply {}: {} file(s).",
                    output.status, output.preview.file_count
                ),
                json!({
                    "evidence_kind": "patch_apply",
                    "operation": "patch_apply",
                    "result": output_json,
                }),
            )?;
            if let (Some(store), Some(run_id), Some(reference)) =
                (&evidence.store, &evidence.run_id, &evidence_ref)
            {
                record_patch_apply_event(
                    &RunStore::new(store.clone()),
                    &RunId::from_string(run_id.clone()),
                    &output,
                    reference,
                )?;
            }
            print_tool_output(serde_json::to_value(&output)?, evidence_ref)?;
        }
        Command::Tools {
            command:
                ToolsCommand::RunCommand {
                    repo,
                    cwd,
                    timeout_seconds,
                    max_output_bytes,
                    source,
                    sandbox,
                    approved,
                    argv,
                    evidence,
                },
        } => {
            let output = run_command(
                &repo,
                CommandRunRequest {
                    cwd,
                    argv,
                    stdin: None,
                    timeout_seconds,
                    max_output_bytes,
                    source,
                    sandbox,
                    approved,
                },
            )?;
            let output_json = serde_json::to_value(&output)?;
            let evidence_ref = write_optional_repo_evidence(
                &evidence,
                RepoEvidenceKind::RepoTest,
                &repo,
                format!("Command {}: {}.", output.status, output.command),
                json!({
                    "evidence_kind": "command_evidence",
                    "operation": "run_command",
                    "result": output_json,
                }),
            )?;
            if let (Some(store), Some(run_id), Some(reference)) =
                (&evidence.store, &evidence.run_id, &evidence_ref)
            {
                record_command_events(
                    &RunStore::new(store.clone()),
                    &RunId::from_string(run_id.clone()),
                    &output,
                    reference,
                )?;
            }
            print_tool_output(serde_json::to_value(&output)?, evidence_ref)?;
        }
        Command::Server { host, port, store } => {
            let addr: SocketAddr = format!("{host}:{port}").parse()?;
            println!("coder server listening on http://{addr}");
            serve(addr, ApiState::new(RunStore::new(store))).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_default_store_roots_use_product_directory() {
        let cli = Cli::parse_from(["coder-rust", "server"]);
        match cli.command {
            Command::Server { store, .. } => assert_eq!(store, PathBuf::from(DEFAULT_STORE)),
            _ => panic!("expected server command"),
        }

        let cli = Cli::parse_from([
            "coder-rust",
            "workflow",
            "run",
            "--mock",
            "planner-led",
            "summarize",
        ]);
        match cli.command {
            Command::Workflow {
                command: WorkflowCommand::Run { store, .. },
            } => assert_eq!(store, PathBuf::from(DEFAULT_STORE)),
            _ => panic!("expected workflow run command"),
        }

        let cli = Cli::parse_from(["coder-rust", "runs", "list"]);
        match cli.command {
            Command::Runs {
                command: RunsCommand::List { store },
            } => assert_eq!(store, PathBuf::from(DEFAULT_STORE)),
            _ => panic!("expected runs list command"),
        }
    }

    #[test]
    fn workflow_preview_reports_ready_with_backends() {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();

        let preview = workflow_preview_json(&config, "planner-led", "summarize the repo");

        assert_eq!(preview["status"], "ready");
        assert_eq!(preview["requires_confirmation"], true);
        assert!(preview["backends"]
            .as_array()
            .unwrap()
            .iter()
            .any(|backend| backend.as_str() == Some("native-rust")));
        assert_eq!(preview["issues"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn workflow_preview_blocks_missing_workflow_and_empty_task() {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();

        let preview = workflow_preview_json(&config, "missing", "  ");
        let codes = preview["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["code"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(preview["status"], "blocked");
        assert_eq!(preview["requires_confirmation"], false);
        assert!(codes.contains(&"workflow_not_found"));
        assert!(codes.contains(&"task_empty"));
    }

    #[test]
    fn optional_repo_evidence_writes_payload_when_store_and_run_id_are_set() {
        let repo = temp_root("coder-cli-repo");
        let store_root = temp_root("coder-cli-store");
        std::fs::create_dir_all(&repo).unwrap();
        let args = EvidenceRecordArgs {
            store: Some(store_root.clone()),
            run_id: Some("run-1".to_owned()),
        };

        let reference = write_optional_repo_evidence(
            &args,
            RepoEvidenceKind::RepoRead,
            &repo,
            "Read src/app.py.",
            json!({
                "evidence_kind": "repo_evidence",
                "operation": "read_file_range",
                "snippet": {"path": "src/app.py", "text": "safe"}
            }),
        )
        .unwrap()
        .unwrap();
        let payload = RunStore::new(&store_root)
            .read_repo_evidence(&reference.ref_id)
            .unwrap();

        assert!(reference.ref_id.starts_with("repo-read:"));
        assert_eq!(payload["operation"], "read_file_range");
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(store_root);
    }

    #[test]
    fn optional_repo_evidence_requires_store_and_run_id_together() {
        let repo = temp_root("coder-cli-repo");
        std::fs::create_dir_all(&repo).unwrap();
        let args = EvidenceRecordArgs {
            store: Some(temp_root("coder-cli-store")),
            run_id: None,
        };

        let error = write_optional_repo_evidence(
            &args,
            RepoEvidenceKind::RepoRead,
            &repo,
            "bad",
            json!({"snippet": "safe"}),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("use --store and --run-id together"));
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn run_list_and_detail_helpers_return_stored_run_json() {
        let store_root = temp_root("coder-cli-store");
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string("run-1");
        let mut state = RunState::new(run_id.clone(), WorkflowId::new("workflow"));
        state.status = RunStatus::Completed;
        store.write_metadata(&state).unwrap();
        store
            .append_event(
                &run_id,
                &CoderEvent::new(run_id.clone(), 1, "run.started", json!({})),
            )
            .unwrap();
        store
            .write_report(&run_id, &coder_core::FinalReport::completed("done"))
            .unwrap();

        let list = run_list_json(&store).unwrap();
        let detail = run_detail_json(&store, &run_id).unwrap();

        assert_eq!(list["runs"][0]["run_id"], "run-1");
        assert_eq!(list["runs"][0]["metadata"]["status"], "completed");
        assert_eq!(detail["run_id"], "run-1");
        assert_eq!(detail["events"][0]["kind"], "run.started");
        assert_eq!(detail["report"]["summary"], "done");
        assert_eq!(detail["repo_evidence_count"], 0);
        let _ = std::fs::remove_dir_all(store_root);
    }

    #[test]
    fn run_detail_helper_returns_repo_evidence_only_run() {
        let store_root = temp_root("coder-cli-store");
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string("run-1");
        store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoRead,
                "repo",
                Vec::new(),
                "read",
                json!({"snippet": "safe"}),
            )
            .unwrap();

        let detail = run_detail_json(&store, &run_id).unwrap();

        assert_eq!(detail["run_id"], "run-1");
        assert_eq!(detail["metadata"], serde_json::Value::Null);
        assert_eq!(detail["repo_evidence_count"], 1);
        let _ = std::fs::remove_dir_all(store_root);
    }

    #[test]
    fn run_repo_evidence_helper_lists_index_records() {
        let store_root = temp_root("coder-cli-store");
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string("run-1");
        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoTextSearch,
                "repo",
                vec!["src".to_owned()],
                "Found one hit.",
                json!({"hits": [{"path": "src/app.py", "line": 1}]}),
            )
            .unwrap();

        let output = run_repo_evidence_json(&store, &run_id).unwrap();

        assert_eq!(output["run_id"], "run-1");
        assert_eq!(output["evidence"][0]["ref_id"], reference.ref_id);
        assert_eq!(output["evidence"][0]["summary"], "Found one hit.");
        let _ = std::fs::remove_dir_all(store_root);
    }

    #[test]
    fn run_report_helper_previews_and_writes_evidence_report() {
        let store_root = temp_root("coder-cli-store");
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string("run-1");
        store
            .append_event(
                &run_id,
                &CoderEvent::new(
                    run_id.clone(),
                    1,
                    "command.completed",
                    json!({
                        "command": "cargo test",
                        "status": "completed",
                        "passed": true,
                        "returncode": 0
                    }),
                ),
            )
            .unwrap();

        let preview = run_report_json(&store, &run_id, false).unwrap();
        let written = run_report_json(&store, &run_id, true).unwrap();

        assert_eq!(preview["report_ref"], serde_json::Value::Null);
        assert_eq!(preview["report"]["status"], "completed");
        assert!(written["report_ref"]
            .as_str()
            .unwrap()
            .ends_with("/final-report.json"));
        assert_eq!(store.read_report(&run_id).unwrap().unwrap().checks.len(), 1);
        let _ = std::fs::remove_dir_all(store_root);
    }

    #[test]
    fn run_report_helper_includes_patch_preview_event_files() {
        let repo = temp_root("coder-cli-repo");
        let store_root = temp_root("coder-cli-store");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(
            repo.join("change.patch"),
            "\
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
        )
        .unwrap();
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string("run-1");
        let args = EvidenceRecordArgs {
            store: Some(store_root.clone()),
            run_id: Some(run_id.to_string()),
        };
        let output =
            preview_patch_file(&repo, "change.patch", coder_tools::DEFAULT_MAX_PATCH_BYTES)
                .unwrap();
        let reference = write_optional_repo_evidence(
            &args,
            RepoEvidenceKind::RepoDiff,
            &repo,
            format!("Previewed patch touching {} file(s).", output.file_count),
            json!({
                "operation": "patch_preview",
                "preview": serde_json::to_value(&output).unwrap()
            }),
        )
        .unwrap()
        .unwrap();
        record_patch_preview_event(&store, &run_id, "change.patch", &output, &reference).unwrap();

        let preview = run_report_json(&store, &run_id, false).unwrap();

        assert_eq!(preview["report"]["changed_files"][0], "tracked.txt");
        assert_eq!(
            preview["report"]["patch_refs"][0],
            format!("repo-evidence://{}", reference.ref_id)
        );
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(store_root);
    }

    #[test]
    fn run_report_helper_blocks_on_patch_apply_approval_event() {
        let repo = temp_root("coder-cli-repo");
        let store_root = temp_root("coder-cli-store");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(
            repo.join("change.patch"),
            "\
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
        )
        .unwrap();
        let store = RunStore::new(&store_root);
        let run_id = RunId::from_string("run-1");
        let output = apply_patch_file(
            &repo,
            PatchApplyRequest {
                patch_file: PathBuf::from("change.patch"),
                max_patch_bytes: coder_tools::DEFAULT_MAX_PATCH_BYTES,
                source: "model".to_owned(),
                approved: false,
            },
        )
        .unwrap();
        let reference = store
            .write_repo_evidence(
                &run_id,
                RepoEvidenceKind::RepoDiff,
                repo.display().to_string(),
                Vec::new(),
                "Patch apply blocked.",
                json!({
                    "operation": "patch_apply",
                    "result": serde_json::to_value(&output).unwrap()
                }),
            )
            .unwrap();
        record_patch_apply_event(&store, &run_id, &output, &reference).unwrap();

        let preview = run_report_json(&store, &run_id, false).unwrap();

        assert_eq!(preview["report"]["status"], "blocked");
        assert_eq!(preview["report"]["changed_files"][0], "tracked.txt");
        assert_eq!(
            preview["report"]["patch_refs"][0],
            format!("repo-evidence://{}", reference.ref_id)
        );
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(store_root);
    }

    #[test]
    fn cli_exposes_phase10_command_surface() {
        let command = Cli::command();
        let root = subcommand_names(&command);

        assert!(root.contains(&"doctor"));
        assert!(root.contains(&"config"));
        assert!(root.contains(&"workflow"));
        assert!(root.contains(&"runs"));
        assert!(root.contains(&"server"));
        assert!(root.contains(&"tools"));

        let workflow = find_subcommand(&command, "workflow");
        let workflow_commands = subcommand_names(workflow);
        assert!(workflow_commands.contains(&"validate"));
        assert!(workflow_commands.contains(&"preview"));
        assert!(workflow_commands.contains(&"run"));
        let workflow_run = find_subcommand(workflow, "run");
        assert!(arg_names(workflow_run).contains(&"repo"));

        let runs = find_subcommand(&command, "runs");
        let runs_commands = subcommand_names(runs);
        assert!(runs_commands.contains(&"list"));
        assert!(runs_commands.contains(&"show"));
    }

    #[test]
    fn run_detail_helper_reports_missing_run() {
        let store_root = temp_root("coder-cli-store");
        let store = RunStore::new(&store_root);

        let error = run_detail_json(&store, &RunId::from_string("missing")).unwrap_err();

        assert!(error.to_string().contains("run 'missing' was not found"));
        let _ = std::fs::remove_dir_all(store_root);
    }

    fn subcommand_names(command: &clap::Command) -> std::collections::BTreeSet<&str> {
        command
            .get_subcommands()
            .map(clap::Command::get_name)
            .collect()
    }

    fn find_subcommand<'a>(command: &'a clap::Command, name: &str) -> &'a clap::Command {
        command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == name)
            .unwrap()
    }

    fn arg_names(command: &clap::Command) -> std::collections::BTreeSet<&str> {
        command
            .get_arguments()
            .map(|argument| argument.get_id().as_str())
            .collect()
    }

    fn temp_root(prefix: &str) -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        test_tmp_root().join(format!("{}-{}-{}", prefix, std::process::id(), id))
    }

    fn test_tmp_root() -> PathBuf {
        std::env::var_os("CODER_TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }
}
