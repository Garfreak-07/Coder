import {
  CheckCircle2,
  ChevronDown,
  KeyRound,
  Network,
  RefreshCw,
  Save,
  ServerCog,
  TestTube2,
  Trash2,
  WandSparkles,
  XCircle
} from "lucide-react";
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
    <div className="provider-settings">
      <section className="settings-group" aria-labelledby="model-settings-heading">
        <header className="settings-group-header">
          <span className="settings-group-icon" aria-hidden="true">
            <ServerCog size={18} strokeWidth={1.8} />
          </span>
          <div>
            <h3 id="model-settings-heading">Model</h3>
            <p>Select the provider endpoint used for conversations and code tasks.</p>
          </div>
        </header>
        <div className="settings-field-grid">
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
          <label className="settings-field-wide">
            Base URL
            <input
              placeholder="Use the provider default"
              value={form.base_url}
              onChange={(event) => onChange({ base_url: event.target.value })}
            />
          </label>
        </div>
      </section>

      <section className="settings-group" aria-labelledby="network-settings-heading">
        <header className="settings-group-header">
          <span className="settings-group-icon" aria-hidden="true">
            <Network size={18} strokeWidth={1.8} />
          </span>
          <div>
            <h3 id="network-settings-heading">Network</h3>
            <p>Keep provider traffic direct or route it through the current environment.</p>
          </div>
        </header>
        <div className="settings-field-grid">
          <label>
            Route
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
            Proxy URL
            <input
              placeholder="http://127.0.0.1:7890"
              disabled={form.proxy_mode !== "explicit"}
              value={form.proxy_url}
              onChange={(event) => onChange({ proxy_url: event.target.value })}
            />
          </label>
        </div>
        <details className="advanced-settings">
          <summary>
            <ChevronDown size={16} strokeWidth={1.8} aria-hidden="true" />
            Advanced network controls
          </summary>
          <div className="settings-field-grid">
            <label>
              Request retries
              <input
                type="number"
                min={0}
                max={100}
                step={1}
                value={form.request_max_retries}
                onChange={(event) => onChange({ request_max_retries: Number(event.target.value) })}
              />
            </label>
            <label>
              Stream idle timeout (ms)
              <input
                type="number"
                min={1}
                step={1000}
                value={form.stream_idle_timeout_ms}
                onChange={(event) => onChange({ stream_idle_timeout_ms: Number(event.target.value) })}
              />
            </label>
          </div>
        </details>
      </section>

      <section className="settings-group" aria-labelledby="credential-settings-heading">
        <header className="settings-group-header">
          <span className="settings-group-icon" aria-hidden="true">
            <KeyRound size={18} strokeWidth={1.8} />
          </span>
          <div>
            <h3 id="credential-settings-heading">Credentials</h3>
            <p>The API key is stored by Coder and is never shown again.</p>
          </div>
        </header>
        <div className="credential-row">
          <label>
            API key
            <input
              type="password"
              placeholder={keyState?.configured ? `${keyState.source}: configured` : "Enter an API key"}
              autoComplete="off"
              value={form.api_key}
              onChange={(event) => onChange({ api_key: event.target.value })}
            />
          </label>
          <button
            className="quiet-action"
            disabled={!keyState?.configured && !form.api_key.trim()}
            onClick={onClearKey}
          >
            <Trash2 size={16} strokeWidth={1.8} aria-hidden="true" />
            <span>Clear key</span>
          </button>
        </div>
        {showMockMode && (
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={form.mock_mode}
              onChange={(event) => onChange({ mock_mode: event.target.checked })}
            />
            <span>Use mock output when credentials are missing</span>
          </label>
        )}
      </section>

      {(currentStatus || testResult) && (
        <section className="settings-group provider-health" aria-label="Provider status">
          {currentStatus && (
            <dl className="provider-summary">
              <div>
                <dt>Runtime</dt>
                <dd>{currentStatus.mode}</dd>
              </div>
              <div>
                <dt>Credential</dt>
                <dd>{currentStatus.configured ? currentStatus.credential_source : "Missing"}</dd>
              </div>
              <div>
                <dt>Endpoint</dt>
                <dd>{currentStatus.base_url ?? "Provider default"}</dd>
              </div>
              <div>
                <dt>Network</dt>
                <dd>{providerNetworkLabel(currentStatus.proxy_mode, currentStatus.proxy_url)}</dd>
              </div>
            </dl>
          )}
          {testResult && (
            <div
              className={`provider-test-result ${testResult.ok ? "provider-test-ok" : "provider-test-failed"}`}
              role="status"
            >
              {testResult.ok ? (
                <CheckCircle2 size={19} strokeWidth={1.8} aria-hidden="true" />
              ) : (
                <XCircle size={19} strokeWidth={1.8} aria-hidden="true" />
              )}
              <div>
                <strong>{testResult.ok ? "Connection succeeded" : "Connection failed"}</strong>
                <p>{testResult.message}</p>
                <span>{testResult.mode} · {testResult.model}{testResult.endpoint ? ` · ${testResult.endpoint}` : ""}</span>
              </div>
            </div>
          )}
        </section>
      )}

      <footer className="settings-actions">
        <div>
          <button className="quiet-action" onClick={() => onChange(deepSeekProviderPreset)}>
            <WandSparkles size={16} strokeWidth={1.8} aria-hidden="true" />
            <span>DeepSeek preset</span>
          </button>
          <button className="quiet-action" onClick={onRefresh}>
            <RefreshCw size={16} strokeWidth={1.8} aria-hidden="true" />
            <span>Refresh</span>
          </button>
        </div>
        <div>
          <button onClick={onTest}>
            <TestTube2 size={16} strokeWidth={1.8} aria-hidden="true" />
            <span>Test connection</span>
          </button>
          <button className="primary-action" onClick={onSave}>
            <Save size={16} strokeWidth={1.8} aria-hidden="true" />
            <span>Save changes</span>
          </button>
        </div>
      </footer>
    </div>
  );
}

function providerNetworkLabel(mode: string, proxyUrl?: string | null): string {
  if (mode === "explicit") {
    return proxyUrl ? "explicit proxy" : "explicit proxy missing URL";
  }
  if (mode === "environment") {
    return proxyUrl ? "environment proxy" : "system/environment route";
  }
  return "direct network";
}
