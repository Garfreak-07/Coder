import type { CacheStatusResponse, SkillExtraRoot } from "../../types";

interface PluginSettingsPanelProps {
  cacheStatus: CacheStatusResponse | null;
  roots: SkillExtraRoot[];
}

export function PluginSettingsPanel({ cacheStatus, roots }: PluginSettingsPanelProps) {
  return (
    <section className="plugin-section">
      <div className="panel-title">Settings</div>
      {cacheStatus && (
        <>
          <div className="cache-status-grid">
            <CacheBucket title="Repo index" entries={cacheStatus.repo_index.entries} bytes={cacheStatus.repo_index.bytes} />
            <CacheBucket title="Plugin cache" entries={cacheStatus.plugin_cache.entries} bytes={cacheStatus.plugin_cache.bytes} />
            <CacheBucket title="Skill cache" entries={cacheStatus.skill_cache.entries} bytes={cacheStatus.skill_cache.bytes} />
            <CacheBucket title="Blob store" entries={cacheStatus.blob_store.entries} bytes={cacheStatus.blob_store.bytes} />
          </div>
        </>
      )}
      <div className="plugin-marketplace-list">
        {roots.length === 0 ? (
          <div className="muted">No extra skill roots configured.</div>
        ) : (
          roots.map((root) => (
            <div className="plugin-row" key={root.path}>
              <strong>{root.scope}</strong>
              <span>{root.path}</span>
              <code>{root.enabled ? "enabled" : "disabled"}</code>
            </div>
          ))
        )}
      </div>
    </section>
  );
}

function CacheBucket({ title, entries, bytes }: { title: string; entries: number; bytes: number }) {
  return (
    <div className="cache-bucket">
      <span>{title}</span>
      <strong>{entries}</strong>
      <code>{bytes} bytes</code>
    </div>
  );
}
