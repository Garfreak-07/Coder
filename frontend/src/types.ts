export interface ConversationMessage {
  role: "user" | "assistant" | "system";
  content: string;
}

export interface ConversationSession {
  session_id: string;
  repo?: string | null;
  messages: ConversationMessage[];
  generation: number;
}

export interface ConversationTurnResponse {
  session: ConversationSession;
  turn_id: string;
  status: "completed" | "cancelled" | string;
  assistant_message: string;
}

export interface ConversationTurnControlResponse {
  session_id: string;
  turn_id: string;
  status: string;
}

export interface ProviderKeyState {
  configured: boolean;
  source: string;
}

export interface ProviderNetworkSettings {
  request_max_retries?: number | null;
  stream_max_retries?: number | null;
  stream_idle_timeout_ms?: number | null;
  websocket_connect_timeout_ms?: number | null;
  supports_websockets: boolean;
}

export interface ProviderSettings {
  default_provider: string;
  default_model: string;
  base_urls: Record<string, string>;
  proxy_urls: Record<string, string>;
  proxy_modes: Record<string, string>;
  network: Record<string, ProviderNetworkSettings>;
  api_keys: Record<string, ProviderKeyState>;
  mock_mode: boolean;
}

export interface ProviderStatusItem {
  provider: string;
  configured: boolean;
  credential_configured: boolean;
  credential_source: string;
  base_url?: string | null;
  proxy_url?: string | null;
  proxy_mode: string;
  request_max_retries: number;
  stream_max_retries: number;
  stream_idle_timeout_ms: number;
  websocket_connect_timeout_ms: number;
  supports_websockets: boolean;
  mode: string;
}

export interface ProviderStatus {
  default_provider: string;
  default_model: string;
  mock_mode: boolean;
  default_status: ProviderStatusItem;
  providers: ProviderStatusItem[];
}

export interface ProviderTestResult {
  provider: string;
  ok: boolean;
  mode: string;
  model: string;
  endpoint?: string | null;
  message: string;
}

export interface ProviderFormState {
  default_provider: string;
  default_model: string;
  base_url: string;
  proxy_mode: "direct" | "explicit" | "environment" | string;
  proxy_url: string;
  request_max_retries: number;
  stream_idle_timeout_ms: number;
  api_key: string;
  mock_mode: boolean;
}

export interface HealthStatus {
  status: string;
  tools: string[];
}

export interface SkillSummary {
  id: string;
  name: string;
  version: string;
  description: string;
  category: string;
  risk_level: string;
  publisher: string;
  connectors: string[];
  connector_operations: unknown[];
  trust_level: string;
  enabled: boolean;
  external_effect: boolean;
}

export interface InstalledSkillsPayload {
  skills: SkillSummary[];
  index: unknown;
}

export interface RemoteSkillEntry {
  id: string;
  name: string;
  version: string;
  description: string;
  category: string;
  publisher: string;
  risk_level: string;
  external_effect: boolean;
  requires_connectors: string[];
  connector_operations: unknown[];
  trust_level: string;
}

export interface DiscoverSkillsPayload {
  registry: unknown;
  skills: Array<RemoteSkillEntry & { installed: boolean }>;
}

export interface SkillUpdateInfo {
  skill_id: string;
  installed_version: string;
  available_version?: string | null;
  update_available: boolean;
  auto_update_eligible: boolean;
  pinned_version?: string | null;
  update_policy: string;
  reason?: string;
  trust_level?: string;
}

export interface ExtensionManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  extension_type: string;
  enabled: boolean;
  risk_level: string;
  trust_level: string;
}

export interface PluginManifest extends ExtensionManifest {
  operations: string[];
  requires_preview: boolean;
}

export interface PluginMarketplace {
  name: string;
  url: string;
  enabled: boolean;
}

export interface PluginMarketplaceListResponse {
  marketplaces: PluginMarketplace[];
}

export interface HookSummary {
  id: string;
  trigger: string;
  enabled: boolean;
  description: string;
}

export interface PluginReadResponse {
  plugin: PluginManifest;
  skills: RemoteSkillEntry[];
  mcp_dependencies: unknown[];
  hooks: HookSummary[];
}

export interface SkillExtraRoot {
  path: string;
  scope: string;
  enabled: boolean;
}

export interface CacheBucketStatus {
  entries: number;
  bytes: number;
}

export interface CacheStatusResponse {
  repo_index: CacheBucketStatus;
  plugin_cache: CacheBucketStatus;
  skill_cache: CacheBucketStatus;
  blob_store: CacheBucketStatus;
}
