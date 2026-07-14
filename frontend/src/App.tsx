import { useEffect, useMemo, useState } from "react";
import { Settings2 } from "lucide-react";
import {
  createConversationSession,
  getConversationSession,
  interruptConversationTurn,
  sendConversationTurn,
  steerConversationTurn
} from "./api";
import { AppErrorBoundary } from "./components/AppErrorBoundary";
import { AppSidebar, type AppSection } from "./components/AppSidebar";
import { ProviderSettingsPanel } from "./components/ProviderSettingsPanel";
import { ConversationPage } from "./features/conversation/ConversationPage";
import { useConversationOutput } from "./features/conversation/useConversationOutput";
import { PluginsPage } from "./features/plugins/PluginsPage";
import { useProviderSettings } from "./hooks/useProviderSettings";
import { useRuntimeInfo } from "./hooks/useRuntimeInfo";
import { enUS } from "./i18n";
import type { ConversationSession } from "./types";

const t = enUS;
const conversationSessionKey = "coder_conversation_session_id";

export function App() {
  const [activeSection, setActiveSection] = useState<AppSection>("chat");
  const [status, setStatus] = useState(t.app.defaultStatus);
  const [repo, setRepo] = useState(".");
  const [request, setRequest] = useState("");
  const [conversationSession, setConversationSession] = useState<ConversationSession | null>(null);
  const [conversationLoading, setConversationLoading] = useState(false);
  const conversationOutput = useConversationOutput();
  const { refreshRuntimeInfo } = useRuntimeInfo(setStatus);
  const {
    providerSettings,
    providerStatus,
    providerTestResult,
    providerForm,
    updateProviderForm,
    clearProviderKey,
    refreshProviderInfo,
    persistProviderSettings,
    runProviderTest
  } = useProviderSettings(setStatus);

  useEffect(() => {
    refreshRuntimeInfo();
    refreshProviderInfo();
    const sessionId = window.localStorage.getItem(conversationSessionKey);
    if (sessionId) {
      setStatus("Restoring conversation...");
      getConversationSession(sessionId)
        .then(async (session) => {
          setConversationSession(session);
          await conversationOutput.connect(session.session_id);
          setStatus("Conversation restored.");
        })
        .catch(() => {
          window.localStorage.removeItem(conversationSessionKey);
          setStatus("Ready");
        });
    }
  }, []);

  async function sendConversationMessage() {
    const message = request.trim();
    if (!message) return;
    if (conversationSession && conversationOutput.activeTurnId) {
      try {
        await steerConversationTurn({
          session_id: conversationSession.session_id,
          turn_id: conversationOutput.activeTurnId,
          message
        });
        setRequest("");
        setStatus("Guidance queued for the active turn.");
      } catch (error) {
        setStatus(error instanceof Error ? error.message : String(error));
      }
      return;
    }
    setConversationLoading(true);
    setStatus("Sending message...");
    try {
      let session = conversationSession;
      if (!session) {
        session = await createConversationSession({ repo });
        setConversationSession(session);
        window.localStorage.setItem(conversationSessionKey, session.session_id);
      }
      await conversationOutput.connect(session.session_id);
      setConversationSession({
        ...session,
        messages: [...session.messages, { role: "user", content: message }],
        generation: session.generation + 1
      });
      setRequest("");
      const response = await sendConversationTurn({
        session_id: session.session_id,
        message,
        repo
      });
      setConversationSession(response.session);
      conversationOutput.clearStreamingText();
      setStatus(response.status === "cancelled" ? "Turn interrupted." : "Coder responded.");
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    } finally {
      setConversationLoading(false);
    }
  }

  async function interruptConversation() {
    if (!conversationSession || !conversationOutput.activeTurnId) return;
    try {
      await interruptConversationTurn({
        session_id: conversationSession.session_id,
        turn_id: conversationOutput.activeTurnId
      });
      setStatus("Interrupt requested.");
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  function startNewConversation() {
    if (conversationOutput.activeTurnId) return;
    window.localStorage.removeItem(conversationSessionKey);
    conversationOutput.reset();
    setConversationSession(null);
    setRequest("");
    setStatus("Ready");
  }

  const debugUiEnabled = useMemo(() => {
    if (typeof window === "undefined") return false;
    return (
      new URLSearchParams(window.location.search).get("debug") === "1" ||
      window.localStorage.getItem("coder_debug_ui") === "1"
    );
  }, []);
  const providerSetupRequired = Boolean(providerStatus) &&
    !providerStatus?.mock_mode &&
    providerStatus?.default_status.provider !== "ollama" &&
    !providerStatus?.default_status.credential_configured;
  const providerSetupMessage = providerStatus
    ? `Configure a provider in Settings before starting a conversation. Current provider: ${providerStatus.default_provider} (${providerStatus.default_model}).`
    : "Provider settings are still loading.";

  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">
        Skip to main content
      </a>
      <AppSidebar
        activeSection={activeSection}
        status={status}
        onSectionChange={setActiveSection}
        showExtensions={debugUiEnabled}
      />

      {activeSection === "chat" ? (
        <AppErrorBoundary message="Something went wrong while rendering the conversation.">
          <ConversationPage
            repo={repo}
            request={request}
            loading={conversationLoading}
            session={conversationSession}
            activeTurnId={conversationOutput.activeTurnId}
            streamingText={conversationOutput.streamingText}
            outputConnection={conversationOutput.connectionState}
            laggedEvents={conversationOutput.laggedEvents}
            codeActivity={conversationOutput.lastCodeEvent?.kind ?? null}
            avatarCue={conversationOutput.avatarCue}
            speechAvailable={conversationOutput.speechAvailable}
            speechEnabled={conversationOutput.speechEnabled}
            providerSetupRequired={providerSetupRequired}
            providerSetupMessage={providerSetupMessage}
            onOpenProviderSettings={() => setActiveSection("settings")}
            onRepoChange={setRepo}
            onRequestChange={setRequest}
            onSubmitRequest={sendConversationMessage}
            onInterrupt={interruptConversation}
            onSpeechEnabledChange={conversationOutput.setSpeechEnabled}
            onStopSpeech={conversationOutput.stopSpeech}
            onNewConversation={startNewConversation}
          />
        </AppErrorBoundary>
      ) : activeSection === "extensions" && debugUiEnabled ? (
        <PluginsPage onStatus={setStatus} />
      ) : (
        <main className="settings-page" id="main-content">
          <header className="page-heading">
            <span className="page-heading-icon" aria-hidden="true">
              <Settings2 size={20} strokeWidth={1.8} />
            </span>
            <div>
              <h2>Settings</h2>
              <p>Configure the model provider and network route used by Coder.</p>
            </div>
          </header>
          <section className="settings-surface" aria-label="Provider settings">
            <ProviderSettingsPanel
              form={providerForm}
              showMockMode={debugUiEnabled}
              settings={providerSettings}
              status={providerStatus}
              testResult={providerTestResult}
              onChange={updateProviderForm}
              onClearKey={clearProviderKey}
              onSave={persistProviderSettings}
              onRefresh={refreshProviderInfo}
              onTest={runProviderTest}
            />
          </section>
        </main>
      )}
    </div>
  );
}
