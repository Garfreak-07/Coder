import { useEffect, useRef, useState } from "react";
import { AvatarDriverHub, type AvatarCue, type AvatarDriver } from "./avatarDriver";
import {
  ConversationOutputClient,
  type OutputConnectionState
} from "./conversationOutputClient";
import type { CoderEvent, OutputEnvelope } from "./outputProtocol";
import { BrowserSpeechOutput } from "./speechOutput";

const speechPreferenceKey = "coder_speech_output_enabled";

export function useConversationOutput() {
  const [activeTurnId, setActiveTurnId] = useState<string | null>(null);
  const [streamingText, setStreamingText] = useState("");
  const [connectionState, setConnectionState] = useState<OutputConnectionState>("disconnected");
  const [avatarCue, setAvatarCue] = useState<AvatarCue | null>(null);
  const [lastCodeEvent, setLastCodeEvent] = useState<CoderEvent | null>(null);
  const [laggedEvents, setLaggedEvents] = useState(0);
  const [speechEnabled, setSpeechEnabledState] = useState(() =>
    typeof window !== "undefined" && window.localStorage.getItem(speechPreferenceKey) === "1"
  );
  const speechRef = useRef<BrowserSpeechOutput | null>(null);
  const avatarHubRef = useRef<AvatarDriverHub | null>(null);
  const clientRef = useRef<ConversationOutputClient | null>(null);
  const envelopeHandlerRef = useRef<(envelope: OutputEnvelope) => void>(() => undefined);

  if (!speechRef.current) speechRef.current = new BrowserSpeechOutput();
  if (!avatarHubRef.current) avatarHubRef.current = new AvatarDriverHub();
  if (!clientRef.current) {
    clientRef.current = new ConversationOutputClient({
      onEnvelope: (envelope) => envelopeHandlerRef.current(envelope),
      onConnectionState: setConnectionState,
      onLagged: (skipped) => setLaggedEvents((current) => current + skipped)
    });
  }

  envelopeHandlerRef.current = (envelope) => {
    speechRef.current?.handle(envelope);
    const cue = avatarHubRef.current?.handle(envelope);
    if (cue) setAvatarCue(cue);
    const event = envelope.output;
    switch (event.type) {
      case "turn_started":
        setActiveTurnId(envelope.turn_id ?? null);
        break;
      case "text_started":
        setStreamingText("");
        break;
      case "text_delta":
        setStreamingText((current) => current + event.delta);
        break;
      case "text_completed":
        setStreamingText(event.text);
        break;
      case "turn_completed":
        setActiveTurnId(null);
        break;
      case "turn_cancelled":
        setActiveTurnId(null);
        setStreamingText("");
        break;
      case "code_event":
        setLastCodeEvent(event.event);
        break;
    }
  };

  useEffect(() => {
    speechRef.current?.setEnabled(speechEnabled);
    window.localStorage.setItem(speechPreferenceKey, speechEnabled ? "1" : "0");
  }, [speechEnabled]);

  useEffect(
    () => () => {
      clientRef.current?.disconnect();
      speechRef.current?.stopAll();
      avatarHubRef.current?.dispose();
    },
    []
  );

  return {
    activeTurnId,
    streamingText,
    connectionState,
    avatarCue,
    lastCodeEvent,
    laggedEvents,
    speechAvailable: speechRef.current.available,
    speechEnabled,
    connect: (sessionId: string) => clientRef.current?.connect(sessionId) ?? Promise.resolve(false),
    disconnect: () => clientRef.current?.disconnect(),
    clearStreamingText: () => setStreamingText(""),
    reset: () => {
      clientRef.current?.disconnect();
      speechRef.current?.stopAll();
      avatarHubRef.current?.reset();
      setActiveTurnId(null);
      setStreamingText("");
      setAvatarCue(null);
      setLastCodeEvent(null);
      setLaggedEvents(0);
    },
    setSpeechEnabled: setSpeechEnabledState,
    stopSpeech: () => speechRef.current?.stopAll(),
    registerAvatarDriver: (driver: AvatarDriver) => avatarHubRef.current?.register(driver) ?? (() => undefined)
  };
}
