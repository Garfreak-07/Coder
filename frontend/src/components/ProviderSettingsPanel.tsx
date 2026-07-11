import { deepSeekProviderPreset } from "../hooks/useProviderSettings";
import type {
  ProviderFormState,
  ProviderSettings,
  ProviderStatus,
  ProviderTestResult
} from "../types";

interface ProviderSettingsPanelProps {
  form: ProviderFormState;
  showMockMode?: boolean;
  settings: ProviderSettings | null;
  status: ProviderStatus | null;
  testResult: ProviderTestResult | null;
  onChange: (patch: Partial<ProviderFormState>) => void;
  onClearKey: () => void;
  onSave: () => void;
  onRefresh: () => void;
  onTest: () => void;
}

export function ProviderSettingsPanel({
  form,
  showMockMode = false,
  settings,
  status,
  testResult,
  onChange,
  onClearKey,
  onSave,
  onRefresh,
  onTest
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
          Provider Network
          <select
            value={form.proxy_mode}
            onChange={(event) => onChange({ proxy_mode: event.target.value })}
          >
            <option value="direct">Direct</option>
            <option value="environment">Current environment</option>
            <option value="explicit">Explicit proxy</option>
          </select>
        </label>
        <label>
          Provider Proxy URL
          <input
            placeholder="Optional, e.g. http://127.0.0.1:7890"
            disabled={form.proxy_mode !== "explicit"}
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
            <span>{providerNetworkLabel(currentStatus.proxy_mode, currentStatus.proxy_url)}</span>
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
    </div>
  );
}

function providerNetworkLabel(mode: string, proxyUrl?: string | null): string {
  if (mode === "explicit") {
    return proxyUrl ? "explicit proxy" : "explicit proxy missing URL";
  }
  if (mode === "environment") {
    return proxyUrl ? "environment proxy" : "environment direct";
  }
  return "direct network";
}
