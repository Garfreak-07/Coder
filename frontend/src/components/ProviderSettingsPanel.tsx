import type { ProviderFormState, ProviderSettings, ProviderStatus } from "../types";

interface ProviderSettingsPanelProps {
  form: ProviderFormState;
  settings: ProviderSettings | null;
  status: ProviderStatus | null;
  onChange: (patch: Partial<ProviderFormState>) => void;
  onSave: () => void;
  onRefresh: () => void;
  onTest: () => void;
}

export function ProviderSettingsPanel({
  form,
  settings,
  status,
  onChange,
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
      <label>
        Provider
        <select value={form.default_provider} onChange={(event) => onChange({ default_provider: event.target.value })}>
          {["openai", "deepseek", "openai-compatible", "qwen", "moonshot", "ollama"].map((providerName) => (
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
        API Key
        <input
          type="password"
          placeholder={keyState?.configured ? `${keyState.source}: configured` : "Leave blank to keep current value"}
          value={form.api_key}
          onChange={(event) => onChange({ api_key: event.target.value })}
        />
      </label>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={form.mock_mode}
          onChange={(event) => onChange({ mock_mode: event.target.checked })}
        />
        Use mock output when credentials are missing
      </label>
      {currentStatus && (
        <div className="summary-grid provider-summary">
          <span>{currentStatus.mode}</span>
          <span>{currentStatus.credential_source}</span>
          <span>{currentStatus.configured ? "configured" : "missing"}</span>
          <span>{currentStatus.base_url ?? "default URL"}</span>
        </div>
      )}
      <div className="button-row">
        <button onClick={onSave}>Save</button>
        <button onClick={onTest}>Test</button>
        <button onClick={onRefresh}>Refresh</button>
      </div>
    </div>
  );
}
