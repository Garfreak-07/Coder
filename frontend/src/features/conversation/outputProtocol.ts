export type OutputPriority = "critical" | "high" | "normal" | "low";
export type OutputStreamBehavior = "queue" | "interrupt" | "replace";
export type SpeechTokenKind = "literal" | "special" | "flush";

export interface CoderEvent {
  event_id: string;
  run_id: string;
  sequence: number;
  timestamp: string;
  kind: string;
  payload: unknown;
  refs: Array<{ label: string; uri: string }>;
}

export type OutputEvent =
  | { type: "session_started" }
  | { type: "turn_started" }
  | { type: "turn_completed" }
  | { type: "turn_cancelled"; reason: string }
  | { type: "text_started" }
  | { type: "text_delta"; delta: string }
  | { type: "text_completed"; text: string }
  | {
      type: "speech_intent_started";
      intent_id: string;
      stream_id: string;
      behavior: OutputStreamBehavior;
      priority: number;
    }
  | {
      type: "speech_intent_token";
      intent_id: string;
      stream_id: string;
      sequence: number;
      kind: SpeechTokenKind;
      value?: string;
    }
  | { type: "speech_intent_ended"; intent_id: string; stream_id: string }
  | {
      type: "speech_intent_cancelled";
      intent_id: string;
      stream_id: string;
      reason?: string;
    }
  | { type: "avatar_cue"; emotion?: string; intensity?: number; motion?: string }
  | { type: "code_event"; event: CoderEvent }
  | { type: "error"; message: string; recoverable: boolean };

export interface OutputEnvelope {
  protocol_version: number;
  event_id: string;
  session_id: string;
  turn_id?: string;
  sequence: number;
  timestamp: string;
  source: string;
  priority: OutputPriority;
  output: OutputEvent;
}

export function isOutputEnvelope(value: unknown): value is OutputEnvelope {
  if (!value || typeof value !== "object") return false;
  const envelope = value as Partial<OutputEnvelope>;
  return (
    envelope.protocol_version === 1 &&
    typeof envelope.event_id === "string" &&
    typeof envelope.session_id === "string" &&
    typeof envelope.sequence === "number" &&
    Boolean(envelope.output) &&
    typeof envelope.output === "object" &&
    typeof (envelope.output as { type?: unknown }).type === "string"
  );
}
