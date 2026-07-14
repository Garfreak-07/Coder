import { useEffect, useState } from "react";
import {
  getCacheStatus,
  getHooks,
  getInstalledPlugins,
  getInstalledSkills,
  getPlugin,
  getPluginMarketplaces,
  getPlugins,
  getSkillExtraRoots
} from "../../api";
import type {
  CacheStatusResponse,
  HookSummary,
  PluginMarketplace as Marketplace,
  PluginManifest,
  PluginReadResponse,
  SkillExtraRoot,
  SkillSummary
} from "../../types";
import { SkillsPanel } from "../skills/SkillsPanel";
import { HooksPanel } from "./HooksPanel";
import { InstalledPlugins } from "./InstalledPlugins";
import { McpDependenciesPanel } from "./McpDependenciesPanel";
import { PluginMarketplace } from "./PluginMarketplace";
import { PluginSettingsPanel } from "./PluginSettingsPanel";
import { SkillDetailsPanel } from "./SkillDetailsPanel";

interface PluginsPageProps {
  onStatus: (status: string) => void;
}

export function PluginsPage({ onStatus }: PluginsPageProps) {
  const [marketplaces, setMarketplaces] = useState<Marketplace[]>([]);
  const [plugins, setPlugins] = useState<PluginManifest[]>([]);
  const [installedPlugins, setInstalledPlugins] = useState<PluginManifest[]>([]);
  const [installedSkills, setInstalledSkills] = useState<SkillSummary[]>([]);
  const [selectedPlugin, setSelectedPlugin] = useState<PluginReadResponse | null>(null);
  const [hooks, setHooks] = useState<HookSummary[]>([]);
  const [roots, setRoots] = useState<SkillExtraRoot[]>([]);
  const [cacheStatus, setCacheStatus] = useState<CacheStatusResponse | null>(null);

  useEffect(() => {
    refresh();
  }, []);

  async function refresh() {
    onStatus("Loading plugins and skills...");
    try {
      const [
        marketplacePayload,
        pluginPayload,
        installedPluginPayload,
        installedSkillPayload,
        hookPayload,
        rootsPayload,
        cachePayload
      ] = await Promise.all([
        getPluginMarketplaces(),
        getPlugins(),
        getInstalledPlugins(),
        getInstalledSkills(),
        getHooks(),
        getSkillExtraRoots(),
        getCacheStatus()
      ]);
      setMarketplaces(marketplacePayload.marketplaces);
      setPlugins(pluginPayload.plugins);
      setInstalledPlugins(installedPluginPayload.plugins);
      setInstalledSkills(installedSkillPayload.skills);
      setHooks(hookPayload.hooks);
      setRoots(rootsPayload.roots);
      setCacheStatus(cachePayload);
      onStatus("Plugins and skills loaded.");
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }

  async function selectPlugin(pluginId: string) {
    onStatus(`Loading plugin ${pluginId}...`);
    try {
      const detail = await getPlugin(pluginId);
      setSelectedPlugin(detail);
      onStatus(`Loaded plugin ${pluginId}.`);
    } catch (error) {
      onStatus(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <main className="plugins-page" id="main-content">
      <PluginMarketplace marketplaces={marketplaces} plugins={plugins} />
      <InstalledPlugins
        plugins={installedPlugins}
        skills={installedSkills}
        onSelectPlugin={selectPlugin}
      />
      <SkillDetailsPanel detail={selectedPlugin} />
      <McpDependenciesPanel dependencies={selectedPlugin?.mcp_dependencies ?? []} />
      <HooksPanel hooks={selectedPlugin?.hooks ?? hooks} />
      <PluginSettingsPanel cacheStatus={cacheStatus} roots={roots} />
      <section className="plugin-section">
        <div className="panel-title">Skills</div>
        <SkillsPanel onStatus={onStatus} />
      </section>
    </main>
  );
}
