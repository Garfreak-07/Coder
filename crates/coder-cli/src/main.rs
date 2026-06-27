use std::{collections::BTreeSet, fs, net::SocketAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand};
use coder_config::{
    load_project_config, validate_project_config, ProjectConfig, ValidationIssue, ValidationLevel,
};
use coder_core::{RunId, RunState, RunStatus, WorkflowId};
use coder_events::CoderEvent;
use coder_openhands::{
    normalize_openhands_event, openhands_final_report, OpenHandsClient, OpenHandsServerConfig,
};
use coder_server::{serve, ApiState};
use coder_store::{RepoEvidenceKind, RepoEvidenceRef, RunStore};
use coder_tools::{
    find_files, git_diff, git_status, read_file, read_file_range, search_text, RepoToolConfig,
};
use coder_workflow::MockWorkflowRunner;
use serde_json::json;

#[derive(Debug, Parser)]
#[command(name = "coder-rust")]
#[command(about = "Rust-first Coder control-plane skeleton")]
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
    Openhands {
        #[command(subcommand)]
        command: OpenHandsCommand,
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
        #[arg(long, default_value = ".coder-rust-server")]
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
        #[arg(long)]
        conversation_id: Option<String>,
        #[arg(long)]
        create_payload: Option<PathBuf>,
        #[arg(long, default_value = "examples/coder.yaml")]
        config: PathBuf,
        #[arg(long, default_value = ".coder-rust")]
        store: PathBuf,
        workflow_id: String,
        task: String,
    },
}

#[derive(Debug, Subcommand)]
enum OpenHandsCommand {
    Doctor {
        #[arg(long)]
        server: String,
        #[arg(long)]
        session_api_key_env: Option<String>,
    },
    Run {
        #[arg(long)]
        server: String,
        #[arg(long)]
        session_api_key_env: Option<String>,
        #[arg(long)]
        conversation_id: Option<String>,
        #[arg(long)]
        create_payload: Option<PathBuf>,
        #[arg(long, default_value = ".coder-rust-openhands")]
        store: PathBuf,
        task: String,
    },
}

#[derive(Debug, Subcommand)]
enum RunsCommand {
    List {
        #[arg(long, default_value = ".coder-rust")]
        store: PathBuf,
    },
    Show {
        #[arg(long, default_value = ".coder-rust")]
        store: PathBuf,
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
}

#[derive(Debug, Clone, Args)]
struct EvidenceRecordArgs {
    #[arg(long)]
    store: Option<PathBuf>,
    #[arg(long)]
    run_id: Option<String>,
}

#[derive(Debug)]
struct OpenHandsWorkflowTarget {
    node_id: String,
    harness_id: String,
    server_url: String,
    session_api_key_env: Option<String>,
}

#[derive(Debug)]
struct OpenHandsRecordedRun {
    workflow_id: String,
    node_id: Option<String>,
    harness_id: Option<String>,
    server_url: String,
    session_api_key_env: Option<String>,
    conversation_id: Option<String>,
    create_payload: Option<PathBuf>,
    store: PathBuf,
    task: String,
}

#[derive(Debug)]
struct OpenHandsRecordedRunOutput {
    run_id: RunId,
    conversation_id: String,
    trigger_status: u16,
    already_running: bool,
    captured_events: usize,
    events_written: usize,
    report_ref: String,
    websocket_url: String,
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

fn select_openhands_workflow_target(
    config: &ProjectConfig,
    workflow_id: &str,
) -> anyhow::Result<OpenHandsWorkflowTarget> {
    let workflow = config
        .workflows
        .get(workflow_id)
        .ok_or_else(|| anyhow::anyhow!("workflow '{workflow_id}' was not found"))?;
    for node in &workflow.nodes {
        let harness = config.harnesses.get(&node.harness).ok_or_else(|| {
            anyhow::anyhow!(
                "workflow '{workflow_id}' node '{}' references missing harness '{}'",
                node.id,
                node.harness
            )
        })?;
        if harness.backend == "openhands" {
            let openhands = harness.openhands.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "harness '{}' uses openhands backend without openhands config",
                    node.harness
                )
            })?;
            return Ok(OpenHandsWorkflowTarget {
                node_id: node.id.clone(),
                harness_id: node.harness.clone(),
                server_url: openhands.server_url.clone(),
                session_api_key_env: openhands.session_api_key_env.clone(),
            });
        }
    }
    anyhow::bail!("workflow '{workflow_id}' has no OpenHands-backed node")
}

async fn run_openhands_recorded(
    input: OpenHandsRecordedRun,
) -> anyhow::Result<OpenHandsRecordedRunOutput> {
    let client = OpenHandsClient::new(OpenHandsServerConfig {
        server_url: input.server_url,
        session_api_key_env: input.session_api_key_env,
    });
    let conversation = match (input.conversation_id, input.create_payload) {
        (Some(conversation_id), None) => client.attach_conversation(&conversation_id).await?,
        (None, Some(create_payload)) => {
            let text = fs::read_to_string(&create_payload)?;
            let payload = serde_json::from_str(&text)?;
            client.create_conversation(payload).await?
        }
        (Some(_), Some(_)) => {
            anyhow::bail!("use either --conversation-id or --create-payload, not both");
        }
        (None, None) => {
            anyhow::bail!("OpenHands run requires --conversation-id or --create-payload");
        }
    };

    client
        .send_user_message(&conversation.id, &input.task, Some("coder-rust"))
        .await?;
    let trigger = client.trigger_run(&conversation.id).await?;
    let raw_events = client.fetch_events(&conversation.id, 100).await?;
    let event_count = raw_events.len();

    let run_id = RunId::new();
    let store = RunStore::new(input.store);
    let mut state = RunState::new(run_id.clone(), WorkflowId::new(input.workflow_id.clone()));
    state.status = RunStatus::Running;
    store.write_metadata(&state)?;

    let mut sequence = 1;
    let mut started_payload = json!({
        "workflow_id": input.workflow_id,
        "backend": "openhands",
        "conversation_id": conversation.id.clone(),
        "task": input.task,
        "trigger_status": trigger.status,
        "already_running": trigger.already_running
    });
    if let Some(node_id) = input.node_id {
        started_payload["node_id"] = json!(node_id);
    }
    if let Some(harness_id) = input.harness_id {
        started_payload["harness_id"] = json!(harness_id);
    }
    store.append_event(
        &run_id,
        &CoderEvent::new(run_id.clone(), sequence, "run.started", started_payload),
    )?;
    sequence += 1;

    let mut raw_refs = Vec::new();
    for (index, raw_event) in raw_events.into_iter().enumerate() {
        let raw_text = serde_json::to_string(&raw_event)?;
        let raw_ref = store.write_large_text_ref(&raw_text)?.blob_ref;
        raw_refs.push(raw_ref.clone());
        let event = normalize_openhands_event(
            run_id.clone(),
            sequence + index as u64,
            raw_event,
            Some(raw_ref),
        );
        store.append_event(&run_id, &event)?;
    }
    sequence += event_count as u64;

    let websocket_url = client.events_websocket_url(&conversation.id)?;
    let report = openhands_final_report(
        &run_id,
        &conversation.id,
        &trigger,
        event_count,
        &websocket_url,
        &raw_refs,
    );
    let report_ref = store.write_report(&run_id, &report)?;
    store.append_event(
        &run_id,
        &CoderEvent::new(
            run_id.clone(),
            sequence,
            "report.created",
            json!({"report_ref": report_ref.clone()}),
        ),
    )?;
    sequence += 1;
    store.append_event(
        &run_id,
        &CoderEvent::new(
            run_id.clone(),
            sequence,
            "run.completed",
            json!({
                "status": "completed",
                "report_ref": report_ref.clone(),
                "openhands_events_captured": event_count
            }),
        ),
    )?;
    state.status = RunStatus::Completed;
    store.write_metadata(&state)?;

    Ok(OpenHandsRecordedRunOutput {
        run_id,
        conversation_id: conversation.id,
        trigger_status: trigger.status,
        already_running: trigger.already_running,
        captured_events: event_count,
        events_written: event_count + 3,
        report_ref,
        websocket_url,
    })
}

fn print_openhands_run_output(output: &OpenHandsRecordedRunOutput) {
    println!("run_id={}", output.run_id);
    println!("conversation_id={}", output.conversation_id);
    println!("openhands_run_status={}", output.trigger_status);
    println!("already_running={}", output.already_running);
    println!("openhands_events_captured={}", output.captured_events);
    println!("events_written={}", output.events_written);
    println!("report_ref={}", output.report_ref);
    println!("events_websocket_url={}", output.websocket_url);
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => {
            println!("coder-rust: ok");
            println!("control_plane: rust skeleton");
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
                    conversation_id,
                    create_payload,
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
                let target = select_openhands_workflow_target(&config, &workflow_id)?;
                let output = run_openhands_recorded(OpenHandsRecordedRun {
                    workflow_id,
                    node_id: Some(target.node_id),
                    harness_id: Some(target.harness_id),
                    server_url: target.server_url,
                    session_api_key_env: target.session_api_key_env,
                    conversation_id,
                    create_payload,
                    store,
                    task,
                })
                .await?;
                print_openhands_run_output(&output);
            }
        }
        Command::Openhands {
            command:
                OpenHandsCommand::Doctor {
                    server,
                    session_api_key_env,
                },
        } => {
            let client = OpenHandsClient::new(OpenHandsServerConfig {
                server_url: server,
                session_api_key_env,
            });
            let health = client.health().await?;
            println!("{}", serde_json::to_string_pretty(&health)?);
            if !health.available {
                std::process::exit(1);
            }
        }
        Command::Openhands {
            command:
                OpenHandsCommand::Run {
                    server,
                    session_api_key_env,
                    conversation_id,
                    create_payload,
                    store,
                    task,
                },
        } => {
            let output = run_openhands_recorded(OpenHandsRecordedRun {
                workflow_id: "openhands-cli".to_owned(),
                node_id: None,
                harness_id: None,
                server_url: server,
                session_api_key_env,
                conversation_id,
                create_payload,
                store,
                task,
            })
            .await?;
            print_openhands_run_output(&output);
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
        Command::Server { host, port, store } => {
            let addr: SocketAddr = format!("{host}:{port}").parse()?;
            println!("coder-rust server listening on http://{addr}");
            serve(addr, ApiState::new(RunStore::new(store))).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_openhands_harness_from_example_workflow() {
        let config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();

        let target = select_openhands_workflow_target(&config, "planner-led").unwrap();

        assert_eq!(target.node_id, "executor");
        assert_eq!(target.harness_id, "openhands-code-edit");
        assert_eq!(target.server_url, "http://127.0.0.1:8000");
        assert_eq!(
            target.session_api_key_env.as_deref(),
            Some("SESSION_API_KEY")
        );
    }

    #[test]
    fn reports_when_workflow_has_no_openhands_harness() {
        let mut config: ProjectConfig =
            serde_yaml::from_str(include_str!("../../../examples/coder.yaml")).unwrap();
        config
            .harnesses
            .get_mut("openhands-code-edit")
            .unwrap()
            .backend = "native-rust".to_owned();

        let error = select_openhands_workflow_target(&config, "planner-led").unwrap_err();

        assert!(error.to_string().contains("no OpenHands-backed node"));
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
            .any(|backend| backend.as_str() == Some("openhands")));
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
    fn run_detail_helper_reports_missing_run() {
        let store_root = temp_root("coder-cli-store");
        let store = RunStore::new(&store_root);

        let error = run_detail_json(&store, &RunId::from_string("missing")).unwrap_err();

        assert!(error.to_string().contains("run 'missing' was not found"));
        let _ = std::fs::remove_dir_all(store_root);
    }

    fn temp_root(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{}-{}",
            prefix,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
