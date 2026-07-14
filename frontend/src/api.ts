import type {
  CacheStatusResponse,
  ConversationSession,
  ConversationTurnControlResponse,
  ConversationTurnResponse,
  DiscoverSkillsPayload,
  ExtensionManifest,
  HealthStatus,
  HookSummary,
  InstalledSkillsPayload,
  PluginManifest,
  PluginMarketplaceListResponse,
  PluginReadResponse,
  ProviderSettings,
  ProviderStatus,
  ProviderTestResult,
  SkillUpdateInfo
} from "./types";

const jsonHeaders = {
  "Content-Type": "application/json"
};

const defaultDesktopApiBaseUrl = "http://127.0.0.1:8876";

declare global {
  interface Window {
    CODER_API_BASE_URL?: string;
  }
}

interface RustHealthResponse {
  status: string;
  service?: string;
}

interface RustConversationSession {
  session_id: string;
  repo_root?: string | null;
  turns: Array<{
    role: string;
    content: string;
  }>;
}

interface RustConversationSessionResponse {
  session: RustConversationSession;
}

interface RustConversationTurnResponse {
  session: RustConversationSession;
  turn_id: string;
  status: string;
  assistant_message: string;
}

export function resolveApiUrl(url: string): string {
  if (/^https?:\/\//i.test(url)) return url;
  const baseUrl = configuredApiBaseUrl() || inferredDesktopApiBaseUrl();
  return baseUrl ? `${baseUrl}${url}` : url;
}

function configuredApiBaseUrl(): string {
  const viteEnv = (import.meta as ImportMeta & { env?: Record<string, string | undefined> }).env;
  const windowApiBaseUrl = typeof window === "undefined" ? "" : window.CODER_API_BASE_URL ?? "";
  const value = viteEnv?.VITE_CODER_API_BASE_URL ?? windowApiBaseUrl;
  return value.trim().replace(/\/+$/, "");
}

function inferredDesktopApiBaseUrl(): string {
  if (typeof window === "undefined") return "";
  const protocol = window.location.protocol;
  if (protocol === "http:" || protocol === "https:") return "";
  return defaultDesktopApiBaseUrl;
}

async function requestJson<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(resolveApiUrl(url), init);
  if (!response.ok) {
    const detail = await response.text();
    throw new Error(`${response.status} ${response.statusText}: ${detail}`);
  }
  return (await response.json()) as T;
}

export async function getHealth(): Promise<HealthStatus> {
  const response = await requestJson<RustHealthResponse>("/api/v3/health");
  return {
    status: response.status,
    tools: response.service ? [response.service] : []
  };
}

export function getInstalledSkills(): Promise<InstalledSkillsPayload> {
  return requestJson<InstalledSkillsPayload>("/api/v3/skills/installed");
}

export async function getExtensionPlugins(): Promise<PluginManifest[]> {
  const payload = await requestJson<{ plugins: PluginManifest[] }>("/api/v3/extensions/plugins");
  return payload.plugins;
}

export function discoverSkills(registryUrl: string): Promise<DiscoverSkillsPayload> {
  return requestJson<DiscoverSkillsPayload>(`/api/v3/skills/discover?registry_url=${encodeURIComponent(registryUrl)}`);
}

export function getSkillUpdates(registryUrl: string): Promise<{ updates: SkillUpdateInfo[] }> {
  return requestJson<{ updates: SkillUpdateInfo[] }>(`/api/v3/skills/updates?registry_url=${encodeURIComponent(registryUrl)}`);
}

export function installSkill(skillId: string, registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson("/api/v3/skills/install", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ skill_id: skillId, registry_url: registryUrl })
  });
}

export function updateSkill(skillId: string, registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/update`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ registry_url: registryUrl })
  });
}

export function autoUpdateSkills(registryUrl: string): Promise<Record<string, unknown>> {
  return requestJson("/api/v3/skills/auto-update", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ registry_url: registryUrl })
  });
}

export function enableSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/enable`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function disableSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/disable`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function removeSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}`, {
    method: "DELETE"
  });
}

export function pinSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/pin`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({})
  });
}

export function unpinSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/unpin`, {
    method: "POST",
    headers: jsonHeaders
  });
}

export function rollbackSkill(skillId: string): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/rollback`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({})
  });
}

export function setSkillUpdatePolicy(
  skillId: string,
  updatePolicy: "manual" | "auto_official_low_risk"
): Promise<Record<string, unknown>> {
  return requestJson(`/api/v3/skills/${encodeURIComponent(skillId)}/update-policy`, {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ update_policy: updatePolicy })
  });
}

export async function getProviderSettings(): Promise<ProviderSettings> {
  const payload = await requestJson<{ settings: ProviderSettings }>("/api/v3/providers/settings");
  return payload.settings;
}

export function getProviderStatus(): Promise<ProviderStatus> {
  return requestJson<ProviderStatus>("/api/v3/providers/status");
}

export function saveProviderSettings(input: Record<string, unknown>): Promise<{
  settings: ProviderSettings;
  status: ProviderStatus;
}> {
  return requestJson("/api/v3/providers/settings", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(input)
  });
}

export function testProvider(provider: string): Promise<{
  status: ProviderStatus;
  test: ProviderTestResult;
}> {
  return requestJson("/api/v3/providers/test", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ provider })
  });
}

export function getPluginMarketplaces(): Promise<PluginMarketplaceListResponse> {
  return requestJson<PluginMarketplaceListResponse>("/api/v3/plugins/marketplaces");
}

export function getPlugins(): Promise<{ plugins: PluginManifest[] }> {
  return requestJson<{ plugins: PluginManifest[] }>("/api/v3/plugins");
}

export function getInstalledPlugins(): Promise<{ plugins: PluginManifest[] }> {
  return requestJson<{ plugins: PluginManifest[] }>("/api/v3/plugins/installed");
}

export function getPlugin(pluginId: string): Promise<PluginReadResponse> {
  return requestJson<PluginReadResponse>(`/api/v3/plugins/${encodeURIComponent(pluginId)}`);
}

export function getSkillExtraRoots(): Promise<{ roots: Array<{ path: string; scope: string; enabled: boolean }> }> {
  return requestJson("/api/v3/skills/extra-roots");
}

export function getHooks(): Promise<{ hooks: HookSummary[] }> {
  return requestJson<{ hooks: HookSummary[] }>("/api/v3/hooks");
}

export function getCacheStatus(): Promise<CacheStatusResponse> {
  return requestJson<CacheStatusResponse>("/api/v3/cache/status");
}

export async function createConversationSession(input: { repo?: string }): Promise<ConversationSession> {
  const payload = await requestJson<RustConversationSessionResponse>("/api/v3/conversations", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ repo: input.repo })
  });
  return mapConversationSession(payload.session);
}

export async function getConversationSession(sessionId: string): Promise<ConversationSession> {
  const payload = await requestJson<RustConversationSessionResponse>(
    `/api/v3/conversations/${encodeURIComponent(sessionId)}`
  );
  return mapConversationSession(payload.session);
}

export async function sendConversationTurn(input: {
  session_id: string;
  message: string;
  repo?: string;
}): Promise<ConversationTurnResponse> {
  const payload = await requestJson<RustConversationTurnResponse>(
    `/api/v3/conversations/${encodeURIComponent(input.session_id)}/turn`,
    {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({
        message: input.message,
        repo: input.repo
      })
    }
  );
  return {
    session: mapConversationSession(payload.session),
    turn_id: payload.turn_id,
    status: payload.status,
    assistant_message: payload.assistant_message
  };
}

export function interruptConversationTurn(input: {
  session_id: string;
  turn_id: string;
}): Promise<ConversationTurnControlResponse> {
  return requestJson(
    `/api/v3/conversations/${encodeURIComponent(input.session_id)}/turns/${encodeURIComponent(input.turn_id)}/interrupt`,
    { method: "POST", headers: jsonHeaders, body: JSON.stringify({}) }
  );
}

export function steerConversationTurn(input: {
  session_id: string;
  turn_id: string;
  message: string;
}): Promise<ConversationTurnControlResponse> {
  return requestJson(
    `/api/v3/conversations/${encodeURIComponent(input.session_id)}/turns/${encodeURIComponent(input.turn_id)}/steer`,
    {
      method: "POST",
      headers: jsonHeaders,
      body: JSON.stringify({ message: input.message })
    }
  );
}

export function conversationOutputEventsUrl(sessionId: string) {
  return resolveApiUrl(`/api/v3/conversations/${encodeURIComponent(sessionId)}/events`);
}

function mapConversationSession(session: RustConversationSession): ConversationSession {
  return {
    session_id: session.session_id,
    repo: session.repo_root ?? undefined,
    messages: session.turns.map((turn) => ({
      role: normalizeConversationRole(turn.role),
      content: turn.content
    })),
    generation: session.turns.length
  };
}

function normalizeConversationRole(role: string): "user" | "assistant" | "system" {
  if (role === "assistant" || role === "system") return role;
  return "user";
}
