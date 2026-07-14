import { Blocks, MessageSquare, Settings, SquareTerminal } from "lucide-react";

export const appSections = ["chat", "extensions", "settings"] as const;

export type AppSection = (typeof appSections)[number];
const primaryAppSections = ["chat", "settings"] as const satisfies readonly AppSection[];

interface AppSidebarProps {
  activeSection: AppSection;
  status: string;
  onSectionChange: (section: AppSection) => void;
  showExtensions?: boolean;
}

const sectionLabels: Record<AppSection, string> = {
  chat: "Conversation",
  extensions: "Plugins & Skills",
  settings: "Settings"
};

const sectionIcons = {
  chat: MessageSquare,
  extensions: Blocks,
  settings: Settings
} satisfies Record<AppSection, typeof MessageSquare>;

export function AppSidebar({
  activeSection,
  status,
  onSectionChange,
  showExtensions = false
}: AppSidebarProps) {
  const advancedSections: readonly AppSection[] = showExtensions ? ["extensions"] : [];
  const advancedOpen = activeSection === "extensions";

  return (
    <aside className="app-sidebar">
      <div className="sidebar-brand">
        <span className="brand-mark" aria-hidden="true">
          <SquareTerminal size={20} strokeWidth={1.8} />
        </span>
        <div>
          <h1>Coder</h1>
          <span>Local runtime</span>
        </div>
      </div>
      <nav className="side-nav" aria-label="Primary">
        {primaryAppSections.map((section) => {
          const Icon = sectionIcons[section];
          return (
            <button
              aria-current={activeSection === section ? "page" : undefined}
              className={activeSection === section ? "selected" : ""}
              key={section}
              onClick={() => onSectionChange(section)}
            >
              <Icon size={18} strokeWidth={1.8} aria-hidden="true" />
              <span>{sectionLabels[section]}</span>
            </button>
          );
        })}
        {advancedSections.length > 0 && (
          <details className="advanced-nav" open={advancedOpen}>
            <summary>Advanced</summary>
            <div className="nav-group-label">Developer</div>
            {advancedSections.map((section) => {
              const Icon = sectionIcons[section];
              return (
                <button
                  aria-current={activeSection === section ? "page" : undefined}
                  className={activeSection === section ? "selected" : ""}
                  key={section}
                  onClick={() => onSectionChange(section)}
                >
                  <Icon size={18} strokeWidth={1.8} aria-hidden="true" />
                  <span>{sectionLabels[section]}</span>
                </button>
              );
            })}
          </details>
        )}
      </nav>
      <div className="sidebar-status" role="status" title={status}>
        <span className="status-dot" aria-hidden="true" />
        <span>{status}</span>
      </div>
    </aside>
  );
}
