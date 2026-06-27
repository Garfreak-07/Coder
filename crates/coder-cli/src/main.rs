use std::{fs, path::PathBuf};

use clap::{Parser, Subcommand};
use coder_config::{load_project_config, validate_project_config};
use coder_core::RunId;
use coder_openhands::{normalize_openhands_event, OpenHandsClient, OpenHandsServerConfig};
use coder_store::RunStore;
use coder_workflow::MockWorkflowRunner;

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
                    config,
                    store,
                    workflow_id,
                    task,
                },
        } => {
            if !mock {
                anyhow::bail!("only --mock workflow runs are implemented in this skeleton");
            }
            let config = load_project_config(&config)?;
            let runner = MockWorkflowRunner::new(&config, RunStore::new(store));
            let output = runner.run(&workflow_id, &task)?;
            println!("run_id={}", output.run_id);
            println!("report_ref={}", output.report_ref);
            println!("summary={}", output.report.summary);
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
            let client = OpenHandsClient::new(OpenHandsServerConfig {
                server_url: server,
                session_api_key_env,
            });
            let conversation = match (conversation_id, create_payload) {
                (Some(conversation_id), None) => {
                    client.attach_conversation(&conversation_id).await?
                }
                (None, Some(create_payload)) => {
                    let text = fs::read_to_string(&create_payload)?;
                    let payload = serde_json::from_str(&text)?;
                    client.create_conversation(payload).await?
                }
                (Some(_), Some(_)) => {
                    anyhow::bail!("use either --conversation-id or --create-payload, not both");
                }
                (None, None) => {
                    anyhow::bail!("openhands run requires --conversation-id or --create-payload");
                }
            };

            client
                .send_user_message(&conversation.id, &task, Some("coder-rust"))
                .await?;
            let trigger = client.trigger_run(&conversation.id).await?;
            let raw_events = client.fetch_events(&conversation.id, 100).await?;
            let event_count = raw_events.len();

            let run_id = RunId::new();
            let store = RunStore::new(store);
            for (index, raw_event) in raw_events.into_iter().enumerate() {
                let raw_text = serde_json::to_string(&raw_event)?;
                let raw_ref = store.write_large_text_ref(&raw_text)?.blob_ref;
                let event = normalize_openhands_event(
                    run_id.clone(),
                    (index + 1) as u64,
                    raw_event,
                    Some(raw_ref),
                );
                store.append_event(&run_id, &event)?;
            }

            println!("run_id={run_id}");
            println!("conversation_id={}", conversation.id);
            println!("openhands_run_status={}", trigger.status);
            println!("already_running={}", trigger.already_running);
            println!("events_written={event_count}");
            println!(
                "events_websocket_url={}",
                client.events_websocket_url(&conversation.id)?
            );
        }
    }
    Ok(())
}
