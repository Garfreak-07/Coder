use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use coder_harness::{
    McpManifestOperation, McpServerManifest, McpServerSummary, McpToolSummary, RiskLevel,
    SideEffectLevel,
};
use rmcp::{
    model::{CallToolRequestParams, ClientInfo, JsonObject, Tool},
    service::RunningService,
    transport::child_process::TokioChildProcess,
    RoleClient, ServiceExt,
};
use serde_json::Value;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{Mutex, RwLock},
    time::timeout,
};

pub const DEFAULT_MCP_STARTUP_TIMEOUT_SECONDS: u64 = 30;
pub const DEFAULT_MCP_TOOL_TIMEOUT_SECONDS: u64 = 300;
const MCP_SHUTDOWN_TIMEOUT_SECONDS: u64 = 3;

type McpService = RunningService<RoleClient, ClientInfo>;

#[derive(Debug, Clone, PartialEq)]
pub struct StdioMcpCallOutput {
    pub output: Value,
    pub is_error: bool,
}

#[derive(Debug, Error)]
pub enum StdioMcpError {
    #[error("MCP server '{0}' is not registered")]
    ServerNotFound(String),
    #[error("MCP tool '{tool}' was not discovered on server '{server}'")]
    ToolNotFound { server: String, tool: String },
    #[error("MCP tool arguments must be a JSON object")]
    InvalidArguments,
    #[error("MCP server '{server}' failed to start: {reason}")]
    Startup { server: String, reason: String },
    #[error("MCP server '{server}' startup timed out after {seconds} seconds")]
    StartupTimeout { server: String, seconds: u64 },
    #[error("MCP server '{server}' tools/list failed: {reason}")]
    ListTools { server: String, reason: String },
    #[error("MCP server '{server}' tools/list timed out after {seconds} seconds")]
    ListToolsTimeout { server: String, seconds: u64 },
    #[error("MCP tool '{tool}' on server '{server}' failed: {reason}")]
    Call {
        server: String,
        tool: String,
        reason: String,
    },
    #[error("MCP tool '{tool}' on server '{server}' timed out after {seconds} seconds")]
    CallTimeout {
        server: String,
        tool: String,
        seconds: u64,
    },
    #[error("failed to serialize MCP result: {0}")]
    Serialize(String),
}

struct StdioMcpConnection {
    manifest: McpServerManifest,
    tools: Vec<McpToolSummary>,
    tool_timeout: Duration,
    service: Mutex<McpService>,
}

#[derive(Clone, Default)]
pub struct StdioMcpRuntime {
    connections: Arc<RwLock<BTreeMap<String, Arc<StdioMcpConnection>>>>,
}

impl std::fmt::Debug for StdioMcpRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StdioMcpRuntime")
            .finish_non_exhaustive()
    }
}

impl StdioMcpRuntime {
    pub async fn register(
        &self,
        manifest: McpServerManifest,
    ) -> Result<McpServerSummary, StdioMcpError> {
        let connection = Arc::new(start_connection(manifest).await?);
        let summary = connection_summary(&connection);
        let replaced = self
            .connections
            .write()
            .await
            .insert(summary.server_id.clone(), connection);
        if let Some(replaced) = replaced {
            close_connection(&replaced).await;
        }
        Ok(summary)
    }

    pub async fn remove(&self, server_id: &str) -> bool {
        let removed = self.connections.write().await.remove(server_id);
        if let Some(removed) = removed {
            close_connection(&removed).await;
            true
        } else {
            false
        }
    }

    pub async fn list_servers(&self) -> Vec<McpServerSummary> {
        self.connections
            .read()
            .await
            .values()
            .map(|connection| connection_summary(connection))
            .collect()
    }

    pub async fn list_tools(&self) -> Vec<McpToolSummary> {
        self.connections
            .read()
            .await
            .values()
            .flat_map(|connection| connection.tools.clone())
            .collect()
    }

    pub async fn find_tool(&self, server_id: &str, tool_name: &str) -> Option<McpToolSummary> {
        self.connections
            .read()
            .await
            .get(server_id)
            .and_then(|connection| {
                connection
                    .tools
                    .iter()
                    .find(|tool| tool.name == tool_name)
                    .cloned()
            })
    }

    pub async fn call_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<StdioMcpCallOutput, StdioMcpError> {
        let connection = self
            .connections
            .read()
            .await
            .get(server_id)
            .cloned()
            .ok_or_else(|| StdioMcpError::ServerNotFound(server_id.to_owned()))?;
        if !connection.tools.iter().any(|tool| tool.name == tool_name) {
            return Err(StdioMcpError::ToolNotFound {
                server: server_id.to_owned(),
                tool: tool_name.to_owned(),
            });
        }
        let arguments = match arguments {
            Value::Object(arguments) => arguments,
            Value::Null => JsonObject::new(),
            _ => return Err(StdioMcpError::InvalidArguments),
        };
        let request = CallToolRequestParams::new(tool_name.to_owned()).with_arguments(arguments);
        let service = connection.service.lock().await;
        let response = timeout(connection.tool_timeout, service.call_tool(request))
            .await
            .map_err(|_| StdioMcpError::CallTimeout {
                server: server_id.to_owned(),
                tool: tool_name.to_owned(),
                seconds: connection.tool_timeout.as_secs(),
            })?
            .map_err(|error| StdioMcpError::Call {
                server: server_id.to_owned(),
                tool: tool_name.to_owned(),
                reason: error.to_string(),
            })?;
        let is_error = response.is_error.unwrap_or(false);
        let output = serde_json::to_value(response)
            .map_err(|error| StdioMcpError::Serialize(error.to_string()))?;
        Ok(StdioMcpCallOutput { output, is_error })
    }
}

async fn start_connection(
    manifest: McpServerManifest,
) -> Result<StdioMcpConnection, StdioMcpError> {
    let startup_seconds = manifest
        .startup_timeout_sec
        .unwrap_or(DEFAULT_MCP_STARTUP_TIMEOUT_SECONDS);
    let tool_seconds = manifest
        .tool_timeout_sec
        .unwrap_or(DEFAULT_MCP_TOOL_TIMEOUT_SECONDS);
    let startup_timeout = Duration::from_secs(startup_seconds);
    let mut command = tokio::process::Command::new(&manifest.command);
    command
        .args(&manifest.args)
        .kill_on_drop(true)
        .env_clear()
        .envs(mcp_environment(&manifest.env_vars));
    if let Some(cwd) = manifest.cwd.as_deref() {
        command.current_dir(cwd);
    }
    let (transport, stderr) = TokioChildProcess::builder(command)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| StdioMcpError::Startup {
            server: manifest.server_id.clone(),
            reason: error.to_string(),
        })?;
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });
    }
    let client_info = ClientInfo::default();
    let mut service = timeout(startup_timeout, client_info.serve(transport))
        .await
        .map_err(|_| StdioMcpError::StartupTimeout {
            server: manifest.server_id.clone(),
            seconds: startup_seconds,
        })?
        .map_err(|error| StdioMcpError::Startup {
            server: manifest.server_id.clone(),
            reason: error.to_string(),
        })?;
    let tools = match timeout(startup_timeout, service.list_all_tools()).await {
        Ok(Ok(tools)) => tools,
        Ok(Err(error)) => {
            let _ = service
                .close_with_timeout(Duration::from_secs(MCP_SHUTDOWN_TIMEOUT_SECONDS))
                .await;
            return Err(StdioMcpError::ListTools {
                server: manifest.server_id.clone(),
                reason: error.to_string(),
            });
        }
        Err(_) => {
            let _ = service
                .close_with_timeout(Duration::from_secs(MCP_SHUTDOWN_TIMEOUT_SECONDS))
                .await;
            return Err(StdioMcpError::ListToolsTimeout {
                server: manifest.server_id.clone(),
                seconds: startup_seconds,
            });
        }
    };
    let tools = tools
        .into_iter()
        .map(|tool| tool_summary(&manifest.server_id, tool))
        .collect::<Vec<_>>();
    Ok(StdioMcpConnection {
        manifest,
        tools,
        tool_timeout: Duration::from_secs(tool_seconds),
        service: Mutex::new(service),
    })
}

async fn close_connection(connection: &StdioMcpConnection) {
    let mut service = connection.service.lock().await;
    let _ = service
        .close_with_timeout(Duration::from_secs(MCP_SHUTDOWN_TIMEOUT_SECONDS))
        .await;
}

fn connection_summary(connection: &StdioMcpConnection) -> McpServerSummary {
    McpServerSummary {
        server_id: connection.manifest.server_id.clone(),
        name: connection.manifest.name.clone(),
        enabled: true,
        requires_approval: true,
        operations: connection
            .tools
            .iter()
            .map(|tool| McpManifestOperation {
                name: tool.name.clone(),
                description: tool.description.clone(),
                risk: tool.risk,
                side_effect: tool.side_effect,
                enabled_by_default: false,
            })
            .collect(),
    }
}

fn tool_summary(server_id: &str, tool: Tool) -> McpToolSummary {
    let (risk, side_effect) = tool_risk(&tool);
    McpToolSummary {
        server_id: server_id.to_owned(),
        name: tool.name.into_owned(),
        description: tool
            .description
            .map(|value| value.into_owned())
            .unwrap_or_default(),
        risk,
        side_effect,
        enabled: true,
        requires_approval: true,
        input_schema: Value::Object((*tool.input_schema).clone()),
    }
}

fn tool_risk(tool: &Tool) -> (RiskLevel, SideEffectLevel) {
    let annotations = tool.annotations.as_ref();
    if annotations.and_then(|value| value.open_world_hint) == Some(true) {
        (RiskLevel::High, SideEffectLevel::External)
    } else if annotations.and_then(|value| value.read_only_hint) == Some(true) {
        (RiskLevel::Low, SideEffectLevel::Read)
    } else if annotations.and_then(|value| value.destructive_hint) == Some(false) {
        (RiskLevel::Medium, SideEffectLevel::Write)
    } else {
        (RiskLevel::High, SideEffectLevel::Write)
    }
}

fn mcp_environment(explicit_names: &[String]) -> HashMap<OsString, OsString> {
    core_environment_names()
        .iter()
        .copied()
        .chain(explicit_names.iter().map(String::as_str))
        .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
        .collect()
}

#[cfg(windows)]
fn core_environment_names() -> &'static [&'static str] {
    &[
        "PATH",
        "PATHEXT",
        "SHELL",
        "COMSPEC",
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "USERNAME",
        "USERDOMAIN",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMW6432",
        "PROGRAMDATA",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
        "TMPDIR",
        "POWERSHELL",
        "PWSH",
    ]
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::OnceLock,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::*;

    async fn environment_lock() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    fn test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("coder-stdio-mcp-{}-{nonce}", std::process::id()))
    }

    #[cfg(windows)]
    fn test_server(root: &std::path::Path) -> (String, Vec<String>) {
        let path = root.join("server.ps1");
        fs::write(
            &path,
            r#"$ErrorActionPreference = 'Stop'
while ($null -ne ($line = [Console]::In.ReadLine())) {
  if ([string]::IsNullOrWhiteSpace($line)) { continue }
  $message = $line | ConvertFrom-Json
  if ($message.method -eq 'initialize') {
    $result = @{
      protocolVersion = $message.params.protocolVersion
      capabilities = @{ tools = @{ listChanged = $false } }
      serverInfo = @{ name = 'coder-test-mcp'; version = '1.0.0' }
    }
    [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $message.id; result = $result } | ConvertTo-Json -Depth 20 -Compress))
  } elseif ($message.method -eq 'tools/list') {
    $tool = @{
      name = 'echo'
      description = 'Echo a message.'
      inputSchema = @{ type = 'object'; properties = @{ message = @{ type = 'string' } }; required = @('message') }
      annotations = @{ readOnlyHint = $true; openWorldHint = $false }
    }
    [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $message.id; result = @{ tools = @($tool) } } | ConvertTo-Json -Depth 20 -Compress))
  } elseif ($message.method -eq 'tools/call') {
    $payload = @{
      echo = $message.params.arguments.message
      forwarded = $env:CODER_MCP_TEST_VALUE
      unforwarded = $null -ne $env:CODER_MCP_UNFORWARDED
    }
    $result = @{ content = @(@{ type = 'text'; text = $payload.echo }); structuredContent = $payload; isError = $false }
    [Console]::Out.WriteLine((@{ jsonrpc = '2.0'; id = $message.id; result = $result } | ConvertTo-Json -Depth 20 -Compress))
  }
}
"#,
        )
        .unwrap();
        (
            "powershell.exe".to_owned(),
            vec![
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-ExecutionPolicy".to_owned(),
                "Bypass".to_owned(),
                "-File".to_owned(),
                path.display().to_string(),
            ],
        )
    }

    #[cfg(not(windows))]
    fn test_server(root: &std::path::Path) -> (String, Vec<String>) {
        let path = root.join("server.sh");
        fs::write(
            &path,
            r#"while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      protocol=$(printf '%s' "$line" | sed -n 's/.*"protocolVersion":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"%s","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"coder-test-mcp","version":"1.0.0"}}}\n' "$id" "$protocol"
      ;;
    *'"method":"tools/list"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo a message.","inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]},"annotations":{"readOnlyHint":true,"openWorldHint":false}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      if [ -n "${CODER_MCP_UNFORWARDED+x}" ]; then leaked=true; else leaked=false; fi
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"hello"}],"structuredContent":{"echo":"hello","forwarded":"%s","unforwarded":%s},"isError":false}}\n' "$id" "$CODER_MCP_TEST_VALUE" "$leaked"
      ;;
  esac
done
"#,
        )
        .unwrap();
        ("sh".to_owned(), vec![path.display().to_string()])
    }

    #[tokio::test]
    async fn stdio_runtime_discovers_calls_and_closes_real_mcp_server() {
        let _environment_guard = environment_lock().await;
        std::env::set_var("CODER_MCP_TEST_VALUE", "forwarded-value");
        std::env::set_var("CODER_MCP_UNFORWARDED", "must-not-leak");
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        let (command, args) = test_server(&root);
        let runtime = StdioMcpRuntime::default();
        let manifest = McpServerManifest {
            server_id: "local-test".to_owned(),
            name: "Local Test".to_owned(),
            command,
            args,
            cwd: Some(root.display().to_string()),
            env_vars: vec!["CODER_MCP_TEST_VALUE".to_owned()],
            startup_timeout_sec: Some(10),
            tool_timeout_sec: Some(10),
            operations: Vec::new(),
            enabled_by_default: false,
        };

        let server = runtime.register(manifest).await.unwrap();
        assert_eq!(server.server_id, "local-test");
        assert!(server.enabled);
        assert_eq!(server.operations.len(), 1);
        let tools = runtime.list_tools().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].risk, RiskLevel::Low);
        assert_eq!(tools[0].side_effect, SideEffectLevel::Read);
        assert_eq!(tools[0].input_schema["type"], "object");

        let result = runtime
            .call_tool("local-test", "echo", json!({"message": "hello"}))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.output["structuredContent"]["echo"], "hello");
        assert_eq!(
            result.output["structuredContent"]["forwarded"],
            "forwarded-value"
        );
        assert_eq!(result.output["structuredContent"]["unforwarded"], false);
        assert!(runtime.remove("local-test").await);
        assert!(runtime.list_servers().await.is_empty());

        std::env::remove_var("CODER_MCP_TEST_VALUE");
        std::env::remove_var("CODER_MCP_UNFORWARDED");
        let _ = fs::remove_dir_all(root);
    }
}

#[cfg(not(windows))]
fn core_environment_names() -> &'static [&'static str] {
    &[
        "PATH", "SHELL", "TMPDIR", "TEMP", "TMP", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "LOGNAME",
        "USER",
    ]
}
