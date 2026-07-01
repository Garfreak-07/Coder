import { deepSeekProviderPreset } from "../hooks/useProviderSettings";
import type {
  OpenHandsFormState,
  OpenHandsSettings,
  OpenHandsStatus,
  ProviderFormState,
  ProviderSettings,
  ProviderStatus,
  ProviderTestResult
} from "../types";

interface ProviderSettingsPanelProps {
  form: ProviderFormState;
  openHandsForm: OpenHandsFormState;
  openHandsSettings: OpenHandsSettings | null;
  openHandsStatus: OpenHandsStatus | null;
  showMockMode?: boolean;
  settings: ProviderSettings | null;
  status: ProviderStatus | null;
  testResult: ProviderTestResult | null;
  onChange: (patch: Partial<ProviderFormState>) => void;
  onOpenHandsChange: (patch: Partial<OpenHandsFormState>) => void;
  onClearKey: () => void;
  onClearOpenHandsToken: () => void;
  onSave: () => void;
  onSaveOpenHands: () => void;
  onRefresh: () => void;
  onRefreshOpenHands: () => void;
  onTest: () => void;
  onTestOpenHands: () => void;
}

export function ProviderSettingsPanel({
  form,
  openHandsForm,
  openHandsSettings,
  openHandsStatus,
  showMockMode = false,
  settings,
  status,
  testResult,
  onChange,
  onOpenHandsChange,
  onClearKey,
  onClearOpenHandsToken,
  onSave,
  onSaveOpenHands,
  onRefresh,
  onRefreshOpenHands,
  onTest,
  onTestOpenHands
}: ProviderSettingsPanelProps) {
  const provider = form.default_provider.trim().toLowerCase() || "openai";
  const currentStatus =
    status?.providers.find((item) => item.provider === provider) ??
    (status?.default_status.provider === provider ? status.default_status : null);
  const keyState = settings?.api_keys[provider];

  return (
    <div className="form-stack">
      <div className="settings-section">
        <div className="panel-subtitle">Planner Provider</div>
        <label>
          Provider
          <select value={form.default_provider} onChange={(event) => onChange({ default_provider: event.target.value })}>
            {["openai-compatible", "deepseek", "custom"].map((providerName) => (
              <option key={providerName} value={providerName}>
                {providerName}
              </option>
            ))}
          </select>
        </label>
        <label>
          Model
          <input value={form.default_model} onChange={(event) => onChange({ default_model: event.target.value })} />
        </label>
        <label>
          Base URL
          <input
            placeholder="Provider default"
            value={form.base_url}
            onChange={(event) => onChange({ base_url: event.target.value })}
          />
        </label>
        <label>
          Provider Proxy URL
          <input
            placeholder="Optional, e.g. http://127.0.0.1:7890"
            value={form.proxy_url}
            onChange={(event) => onChange({ proxy_url: event.target.value })}
          />
        </label>
        <label>
          API Key
          <input
            type="password"
            placeholder={keyState?.configured ? `${keyState.source}: configured` : "Leave blank to keep current value"}
            autoComplete="off"
            value={form.api_key}
            onChange={(event) => onChange({ api_key: event.target.value })}
          />
        </label>
        {showMockMode && (
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={form.mock_mode}
              onChange={(event) => onChange({ mock_mode: event.target.checked })}
            />
            Use mock output when credentials are missing
          </label>
        )}
        {currentStatus && (
          <div className="summary-grid provider-summary">
            <span>{currentStatus.mode}</span>
            <span>{currentStatus.credential_source}</span>
            <span>{currentStatus.configured ? "configured" : "missing"}</span>
            <span>{currentStatus.base_url ?? "default URL"}</span>
            <span>{currentStatus.proxy_url ? "proxy configured" : "direct network"}</span>
          </div>
        )}
        {testResult && (
          <div className={`provider-test-result ${testResult.ok ? "provider-test-ok" : "provider-test-failed"}`}>
            <strong>{testResult.ok ? "Test succeeded" : "Test failed"}</strong>
            <span>{testResult.mode}</span>
            <span>Model: {testResult.model}</span>
            {testResult.endpoint && <span>Endpoint: {testResult.endpoint}</span>}
            <p>{testResult.message}</p>
          </div>
        )}
        <div className="button-row">
          <button onClick={() => onChange(deepSeekProviderPreset)}>DeepSeek preset</button>
          <button onClick={onSave}>Save</button>
          <button onClick={onTest}>Test Provider</button>
          <button disabled={!keyState?.configured && !form.api_key.trim()} onClick={onClearKey}>
            Clear API Key
          </button>
          <button onClick={onRefresh}>Refresh</button>
        </div>
      </div>

      <div className="settings-section">
        <div className="panel-subtitle">Execution Backend / OpenHands</div>
        <p className="muted">
          OpenHands is the required execution backend for Start Work. If it is unavailable, Planner will block and
          explain what needs to be fixed.
        </p>
        <label>
          Server URL
          <input
            placeholder="http://127.0.0.1:8000"
            value={openHandsForm.server_url}
            onChange={(event) => onOpenHandsChange({ server_url: event.target.value })}
          />
        </label>
        <label>
          Session API key / token
          <input
            type="password"
            placeholder={
              openHandsSettings?.session_api_key.configured
                ? `${openHandsSettings.session_api_key.source}: configured`
                : "Leave blank to keep current value"
            }
            autoComplete="off"
            value={openHandsForm.session_api_key}
            onChange={(event) => onOpenHandsChange({ session_api_key: event.target.value })}
          />
        </label>
        <label>
          Workspace mode
          <select
            value={openHandsForm.workspace_mode}
            onChange={(event) => onOpenHandsChange({ workspace_mode: event.target.value })}
          >
            <option value="local">local</option>
            <option value="ephemeral">ephemeral</option>
          </select>
        </label>
        {openHandsStatus && (
          <div className={`provider-test-result openhands-status-${openHandsStatus.status}`}>
            <strong>OpenHands {openHandsStatus.status}</strong>
            <span>required executor</span>
            <span>{openHandsStatus.credential_source}</span>
            <span>{openHandsStatus.server_url}</span>
            <span>Workspace: {openHandsStatus.workspace_mode}</span>
            {openHandsStatus.version && <span>Version: {openHandsStatus.version}</span>}
            {openHandsStatus.capabilities.length > 0 && (
              <span>Capabilities: {openHandsStatus.capabilities.join(", ")}</span>
            )}
            <p>{openHandsStatus.detail}</p>
          </div>
        )}
        <div className="button-row">
          <button onClick={onSaveOpenHands}>Save OpenHands</button>
          <button onClick={onTestOpenHands}>Test OpenHands</button>
          <button
            disabled={!openHandsSettings?.session_api_key.configured && !openHandsForm.session_api_key.trim()}
            onClick={onClearOpenHandsToken}
          >
            Clear OpenHands Token
          </button>
          <button onClick={onRefreshOpenHands}>Refresh OpenHands</button>
        </div>
      </div>
    </div>
  );
}
