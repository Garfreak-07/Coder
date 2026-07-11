import { useCallback, useState } from "react";

import {
  getProviderSettings,
  getProviderStatus,
  saveProviderSettings,
  testProvider
} from "../api";
import type { ProviderFormState, ProviderSettings, ProviderStatus, ProviderTestResult } from "../types";

export const deepSeekProviderPreset: ProviderFormState = {
  default_provider: "deepseek",
  default_model: "deepseek-v4-flash",
  base_url: "https://api.deepseek.com",
  proxy_mode: "direct",
  proxy_url: "",
  api_key: "",
  mock_mode: false
};

const defaultProviderForm: ProviderFormState = {
  default_provider: deepSeekProviderPreset.default_provider,
  default_model: deepSeekProviderPreset.default_model,
  base_url: deepSeekProviderPreset.base_url,
  proxy_mode: deepSeekProviderPreset.proxy_mode,
  proxy_url: deepSeekProviderPreset.proxy_url,
  api_key: "",
  mock_mode: deepSeekProviderPreset.mock_mode
};

export function useProviderSettings(onStatus: (status: string) => void) {
  const [providerSettings, setProviderSettings] = useState<ProviderSettings | null>(null);
  const [providerStatus, setProviderStatus] = useState<ProviderStatus | null>(null);
  const [providerTestResult, setProviderTestResult] = useState<ProviderTestResult | null>(null);
  const [providerForm, setProviderForm] = useState<ProviderFormState>(defaultProviderForm);

  const refreshProviderInfo = useCallback(() => {
    Promise.all([getProviderSettings(), getProviderStatus()])
      .then(([settings, status]) => {
        setProviderSettings(settings);
        setProviderStatus(status);
        const provider = settings.default_provider || status.default_provider || "openai";
        setProviderForm({
          default_provider: provider,
          default_model: settings.default_model || status.default_model || deepSeekProviderPreset.default_model,
          base_url: settings.base_urls[provider] ?? "",
          proxy_mode: (settings.proxy_modes ?? {})[provider] ?? defaultProxyModeForProvider(provider),
          proxy_url: (settings.proxy_urls ?? {})[provider] ?? "",
          api_key: "",
          mock_mode: settings.mock_mode
        });
      })
      .catch((error) => onStatus(`Failed to load provider settings: ${error.message}`));
  }, [onStatus]);

  const updateProviderForm = useCallback(
    (patch: Partial<ProviderFormState>) => {
      const nextProvider = patch.default_provider?.trim().toLowerCase();
      setProviderForm((current) => {
        const merged = { ...current, ...patch };
        if (nextProvider && nextProvider !== current.default_provider) {
          merged.base_url = providerSettings?.base_urls[nextProvider] ?? defaultBaseUrlForProvider(nextProvider);
          merged.proxy_mode =
            (providerSettings?.proxy_modes ?? {})[nextProvider] ?? defaultProxyModeForProvider(nextProvider);
          merged.proxy_url = (providerSettings?.proxy_urls ?? {})[nextProvider] ?? "";
          merged.api_key = "";
          if (!patch.default_model) {
            merged.default_model = defaultModelForProvider(nextProvider);
          }
        }
        return merged;
      });
    },
    [providerSettings]
  );

  const persistProviderSettings = useCallback(async () => {
    const provider = providerForm.default_provider.trim().toLowerCase() || "openai";
    const payload = buildProviderSettingsPayload(providerForm, providerSettings);
    onStatus(`Saving provider ${provider}...`);
    try {
      const result = await saveProviderSettings(payload);
      setProviderSettings(result.settings);
      setProviderStatus(result.status);
      setProviderTestResult(null);
      setProviderForm((current) => ({ ...current, default_provider: provider, api_key: "" }));
      onStatus(`Provider ${provider} saved.`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }, [onStatus, providerForm, providerSettings]);

  const runProviderTest = useCallback(async () => {
    const provider = providerForm.default_provider.trim().toLowerCase() || "openai";
    const payload = buildProviderSettingsPayload(providerForm, providerSettings);
    onStatus(`Saving provider ${provider} before test...`);
    try {
      const saved = await saveProviderSettings(payload);
      setProviderSettings(saved.settings);
      setProviderStatus(saved.status);
      setProviderForm((current) => ({ ...current, default_provider: provider, api_key: "" }));
      onStatus(`Checking provider ${provider}...`);
      const result = await testProvider(provider);
      setProviderStatus(result.status);
      setProviderTestResult(result.test);
      onStatus(`Provider ${provider}: ${result.test.ok ? "success" : "failed"} - ${result.test.message}`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }, [onStatus, providerForm, providerSettings]);

  const clearProviderKey = useCallback(async () => {
    const provider = providerForm.default_provider.trim().toLowerCase() || "openai";
    onStatus(`Clearing API key for ${provider}...`);
    try {
      const result = await saveProviderSettings({
        api_keys: { [provider]: null }
      });
      setProviderSettings(result.settings);
      setProviderStatus(result.status);
      setProviderTestResult(null);
      setProviderForm((current) => ({ ...current, api_key: "" }));
      onStatus(`API key for ${provider} cleared.`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }, [onStatus, providerForm.default_provider]);

  return {
    providerSettings,
    providerStatus,
    providerTestResult,
    providerForm,
    updateProviderForm,
    refreshProviderInfo,
    persistProviderSettings,
    runProviderTest,
    clearProviderKey
  };
}

function buildProviderSettingsPayload(
  providerForm: ProviderFormState,
  providerSettings: ProviderSettings | null
): Record<string, unknown> {
  const provider = providerForm.default_provider.trim().toLowerCase() || "openai";
  const baseUrls = { ...(providerSettings?.base_urls ?? {}) };
  const proxyUrls = { ...(providerSettings?.proxy_urls ?? {}) };
  const proxyModes = { ...(providerSettings?.proxy_modes ?? {}) };
  if (providerForm.base_url.trim()) {
    baseUrls[provider] = providerForm.base_url.trim();
  } else {
    delete baseUrls[provider];
  }
  const proxyMode = normalizeProviderProxyMode(providerForm.proxy_mode, provider);
  proxyModes[provider] = proxyMode;
  if (proxyMode === "explicit" && providerForm.proxy_url.trim()) {
    proxyUrls[provider] = providerForm.proxy_url.trim();
  } else {
    delete proxyUrls[provider];
  }
  const payload: Record<string, unknown> = {
    default_provider: provider,
    default_model: providerForm.default_model.trim() || "gpt-4.1-mini",
    base_urls: baseUrls,
    proxy_urls: proxyUrls,
    proxy_modes: proxyModes,
    mock_mode: providerForm.mock_mode
  };
  if (providerForm.api_key.trim()) {
    payload.api_keys = { [provider]: providerForm.api_key.trim() };
  }
  return payload;
}

function defaultBaseUrlForProvider(provider: string): string {
  if (provider === deepSeekProviderPreset.default_provider || provider === "deepseek") {
    return deepSeekProviderPreset.base_url;
  }
  return "";
}

function defaultProxyModeForProvider(provider: string): "direct" | "environment" {
  if (provider === deepSeekProviderPreset.default_provider || provider === "deepseek") {
    return "direct";
  }
  return "environment";
}

function normalizeProviderProxyMode(mode: string, provider: string): "direct" | "explicit" | "environment" {
  if (mode === "direct" || mode === "explicit" || mode === "environment") {
    return mode;
  }
  return defaultProxyModeForProvider(provider);
}

function defaultModelForProvider(provider: string): string {
  if (provider === deepSeekProviderPreset.default_provider || provider === "deepseek") {
    return deepSeekProviderPreset.default_model;
  }
  return "gpt-4.1-mini";
}
