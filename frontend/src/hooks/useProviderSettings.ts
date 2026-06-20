import { useCallback, useState } from "react";

import {
  getProviderSettings,
  getProviderStatus,
  saveProviderSettings,
  testProvider
} from "../api";
import type { ProviderFormState, ProviderSettings, ProviderStatus } from "../types";

const defaultProviderForm: ProviderFormState = {
  default_provider: "openai",
  default_model: "gpt-4.1-mini",
  base_url: "",
  api_key: "",
  mock_mode: true
};

export function useProviderSettings(onStatus: (status: string) => void) {
  const [providerSettings, setProviderSettings] = useState<ProviderSettings | null>(null);
  const [providerStatus, setProviderStatus] = useState<ProviderStatus | null>(null);
  const [providerForm, setProviderForm] = useState<ProviderFormState>(defaultProviderForm);

  const refreshProviderInfo = useCallback(() => {
    Promise.all([getProviderSettings(), getProviderStatus()])
      .then(([settings, status]) => {
        setProviderSettings(settings);
        setProviderStatus(status);
        const provider = settings.default_provider || status.default_provider || "openai";
        setProviderForm({
          default_provider: provider,
          default_model: settings.default_model || status.default_model || "gpt-4.1-mini",
          base_url: settings.base_urls[provider] ?? "",
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
          merged.base_url = providerSettings?.base_urls[nextProvider] ?? "";
          merged.api_key = "";
        }
        return merged;
      });
    },
    [providerSettings]
  );

  const persistProviderSettings = useCallback(async () => {
    const provider = providerForm.default_provider.trim().toLowerCase() || "openai";
    const baseUrls = { ...(providerSettings?.base_urls ?? {}) };
    if (providerForm.base_url.trim()) {
      baseUrls[provider] = providerForm.base_url.trim();
    } else {
      delete baseUrls[provider];
    }
    const payload: Record<string, unknown> = {
      default_provider: provider,
      default_model: providerForm.default_model.trim() || "gpt-4.1-mini",
      base_urls: baseUrls,
      mock_mode: providerForm.mock_mode
    };
    if (providerForm.api_key.trim()) {
      payload.api_keys = { [provider]: providerForm.api_key.trim() };
    }
    onStatus(`Saving provider ${provider}...`);
    try {
      const result = await saveProviderSettings(payload);
      setProviderSettings(result.settings);
      setProviderStatus(result.status);
      setProviderForm((current) => ({ ...current, default_provider: provider, api_key: "" }));
      onStatus(`Provider ${provider} saved.`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }, [onStatus, providerForm, providerSettings]);

  const runProviderTest = useCallback(async () => {
    const provider = providerForm.default_provider.trim().toLowerCase() || "openai";
    onStatus(`Checking provider ${provider}...`);
    try {
      const result = await testProvider(provider);
      setProviderStatus(result);
      const item = result.providers[0] ?? result.default_status;
      onStatus(`Provider ${provider}: ${item.mode}, credentials ${item.credential_source}`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }, [onStatus, providerForm.default_provider]);

  return {
    providerSettings,
    providerStatus,
    providerForm,
    updateProviderForm,
    refreshProviderInfo,
    persistProviderSettings,
    runProviderTest
  };
}
