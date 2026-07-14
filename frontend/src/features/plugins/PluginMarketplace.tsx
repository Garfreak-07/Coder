import type { PluginMarketplace as Marketplace, PluginManifest } from "../../types";

interface PluginMarketplaceProps {
  marketplaces: Marketplace[];
  plugins: PluginManifest[];
}

export function PluginMarketplace({ marketplaces, plugins }: PluginMarketplaceProps) {
  return (
    <section className="plugin-section">
      <div className="panel-title">Explore marketplace</div>
      <div className="plugin-marketplace-list">
        {marketplaces.map((marketplace) => (
          <div className="plugin-row" key={marketplace.name}>
            <strong>{marketplace.name}</strong>
            <span>{marketplace.url}</span>
            <code>{marketplace.enabled ? "enabled" : "disabled"}</code>
          </div>
        ))}
      </div>
      <div className="plugin-card-grid">
        {plugins.map((plugin) => (
          <article className="plugin-card" key={plugin.id}>
            <div>
              <strong>{plugin.name}</strong>
              <code>{plugin.version}</code>
            </div>
            <p>{plugin.description}</p>
            <div className="plugin-meta">
              <span>{plugin.trust_level}</span>
              <span>{plugin.risk_level}</span>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
