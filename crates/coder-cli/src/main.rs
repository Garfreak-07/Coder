use std::{fs, net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand};
use coder_config::{load_project_config, validate_project_config, ProjectConfig};
use coder_core::{RunId, RunState, RunStatus, WorkflowId};
use coder_events::CoderEvent;
use coder_openhands::{
    normalize_openhands_event, openhands_final_report, OpenHandsClient, OpenHandsServerConfig,
};
use coder_server::{serve, ApiState};
use coder_store::RunStore;
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
}
