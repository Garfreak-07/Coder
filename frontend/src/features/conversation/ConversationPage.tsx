import { useEffect, useRef } from "react";
import {
  AlertTriangle,
  Folder,
  MessageSquareCode,
  Plus,
  Send,
  Settings,
  Volume2,
  VolumeX,
  X
} from "lucide-react";
import type { ConversationSession } from "../../types";
import type { AvatarCue } from "./avatarDriver";
import type { OutputConnectionState } from "./conversationOutputClient";

interface ConversationPageProps {
  repo: string;
  request: string;
  loading: boolean;
  session: ConversationSession | null;
  activeTurnId: string | null;
  streamingText: string;
  outputConnection: OutputConnectionState;
  laggedEvents: number;
  codeActivity: string | null;
  avatarCue: AvatarCue | null;
  speechAvailable: boolean;
  speechEnabled: boolean;
  providerSetupRequired: boolean;
  providerSetupMessage: string;
  onOpenProviderSettings: () => void;
  onRepoChange: (value: string) => void;
  onRequestChange: (value: string) => void;
  onSubmitRequest: () => void;
  onInterrupt: () => void;
  onSpeechEnabledChange: (enabled: boolean) => void;
  onStopSpeech: () => void;
  onNewConversation: () => void;
}

export function ConversationPage({
  repo,
  request,
  loading,
  session,
  activeTurnId,
  streamingText,
  outputConnection,
  laggedEvents,
  codeActivity,
  avatarCue,
  speechAvailable,
  speechEnabled,
  providerSetupRequired,
  providerSetupMessage,
  onOpenProviderSettings,
  onRepoChange,
  onRequestChange,
  onSubmitRequest,
  onInterrupt,
  onSpeechEnabledChange,
  onStopSpeech,
  onNewConversation
}: ConversationPageProps) {
  const threadRef = useRef<HTMLElement>(null);
  const inputDisabled = providerSetupRequired;
  const canSend =
    request.trim().length > 0 && !inputDisabled && (!loading || Boolean(activeTurnId));
  const messages = session?.messages ?? [];
  const hasMessages = messages.length > 0;

  useEffect(() => {
    const thread = threadRef.current;
    if (thread) thread.scrollTop = thread.scrollHeight;
  }, [messages.length, streamingText, loading]);

  return (
    <main className="chat-page" id="main-content">
      <header className="chat-header">
        <div>
          <h2>Conversation</h2>
          <p>{session ? "Session active" : "New session"}</p>
        </div>
        <div className="chat-header-tools">
          <ProjectControl
            className="desktop-project-control"
            repo={repo}
            onRepoChange={onRepoChange}
          />
          <button
            aria-label="Start a new conversation"
            className="icon-button"
            disabled={Boolean(activeTurnId)}
            onClick={onNewConversation}
            title="New conversation"
          >
            <Plus size={19} strokeWidth={1.8} aria-hidden="true" />
          </button>
        </div>
      </header>
      <section className="chat-thread" aria-label="Conversation" ref={threadRef}>
        {!hasMessages ? (
          <div className="chat-empty">
            {providerSetupRequired && (
              <ProviderSetupBanner
                message={providerSetupMessage}
                onOpenProviderSettings={onOpenProviderSettings}
              />
            )}
            <div className="chat-empty-panel">
              <span className="empty-state-icon" aria-hidden="true">
                <MessageSquareCode size={36} strokeWidth={1.5} />
              </span>
              <h3>What are we building?</h3>
              <p>Ask about your codebase, investigate a problem, or plan the next change.</p>
            </div>
          </div>
        ) : (
          <>
            {providerSetupRequired && (
              <ProviderSetupBanner
                message={providerSetupMessage}
                onOpenProviderSettings={onOpenProviderSettings}
              />
            )}
            {messages.map((message, index) => (
              <article
                key={`${index}-${message.role}`}
                className={`chat-message ${message.role === "user" ? "user-message" : "assistant-message"}`}
              >
                <div className="message-bubble">
                  <div className="message-role">{message.role === "user" ? "You" : "Coder"}</div>
                  <p>{message.content}</p>
                </div>
              </article>
            ))}
            {streamingText && (
              <article className="chat-message assistant-message" aria-live="polite">
                <div className="message-bubble streaming-message">
                  <div className="message-role">Coder</div>
                  <p>{streamingText}</p>
                </div>
              </article>
            )}
            {loading && !streamingText && (
              <div className="chat-loading-row" role="status">
                <span className="loading-dot" aria-hidden="true" />
                Coder is responding...
              </div>
            )}
            {(codeActivity || avatarCue) && (
              <div className="output-activity" aria-live="polite">
                {codeActivity && <span>Code: {codeActivity}</span>}
                {avatarCue?.emotion && <span>Expression: {avatarCue.emotion}</span>}
                {avatarCue?.motion && <span>Motion: {avatarCue.motion}</span>}
              </div>
            )}
          </>
        )}
      </section>

      <section className="chat-composer" aria-label="Conversation input">
        <div className="composer-shell">
          <div className="composer-input-row">
            <MessageSquareCode size={20} strokeWidth={1.6} aria-hidden="true" />
            <textarea
              aria-label="Message Coder"
              value={request}
              disabled={inputDisabled}
              onChange={(event) => onRequestChange(event.target.value)}
              placeholder="Message Coder..."
              rows={3}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  if (canSend) onSubmitRequest();
                }
              }}
            />
            <button
              aria-label={activeTurnId ? "Steer active conversation" : "Send message"}
              className="primary-action send-action"
              disabled={!canSend}
              onClick={onSubmitRequest}
              title={activeTurnId ? "Steer active conversation" : "Send message"}
            >
              <Send size={19} strokeWidth={1.9} aria-hidden="true" />
            </button>
          </div>
          <div className="composer-footer">
            <ProjectControl
              className="mobile-project-control"
              repo={repo}
              onRepoChange={onRepoChange}
            />
            <div className="composer-output-controls">
              <label className="speech-toggle">
                <Volume2 size={16} strokeWidth={1.8} aria-hidden="true" />
                <span>Voice</span>
                <input
                  type="checkbox"
                  checked={speechEnabled}
                  disabled={!speechAvailable}
                  onChange={(event) => onSpeechEnabledChange(event.target.checked)}
                />
                <span className="toggle-track" aria-hidden="true">
                  <span />
                </span>
              </label>
              <span className={`output-connection ${outputConnection}`}>
                <span className="connection-dot" aria-hidden="true" />
                {outputConnection}
              </span>
              {laggedEvents > 0 && <span className="output-lag">{laggedEvents} events skipped</span>}
            </div>
            <div className="composer-actions">
              {speechEnabled && (
                <button
                  aria-label="Stop voice output"
                  className="icon-button quiet-action"
                  onClick={onStopSpeech}
                  title="Stop voice"
                >
                  <VolumeX size={17} strokeWidth={1.8} aria-hidden="true" />
                </button>
              )}
              {activeTurnId && (
                <button className="danger-action" onClick={onInterrupt}>
                  <X size={17} strokeWidth={1.8} aria-hidden="true" />
                  <span>Stop</span>
                </button>
              )}
            </div>
          </div>
        </div>
      </section>
    </main>
  );
}

function ProviderSetupBanner({
  message,
  onOpenProviderSettings
}: {
  message: string;
  onOpenProviderSettings: () => void;
}) {
  return (
    <div className="provider-setup-card" role="status">
      <AlertTriangle size={20} strokeWidth={1.8} aria-hidden="true" />
      <div>
        <strong>Provider setup required</strong>
        <p>{message}</p>
      </div>
      <button onClick={onOpenProviderSettings}>
        <Settings size={17} strokeWidth={1.8} aria-hidden="true" />
        <span>Open settings</span>
      </button>
    </div>
  );
}

function ProjectControl({
  className,
  repo,
  onRepoChange
}: {
  className: string;
  repo: string;
  onRepoChange: (value: string) => void;
}) {
  return (
    <label className={`project-control ${className}`}>
      <Folder size={17} strokeWidth={1.8} aria-hidden="true" />
      <span className="sr-only">Project path</span>
      <input
        aria-label="Project path"
        spellCheck={false}
        value={repo}
        onChange={(event) => onRepoChange(event.target.value)}
      />
    </label>
  );
}
