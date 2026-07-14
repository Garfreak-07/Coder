import { conversationOutputEventsUrl } from "../../api";
import { isOutputEnvelope, type OutputEnvelope } from "./outputProtocol";

export type OutputConnectionState = "disconnected" | "connecting" | "connected" | "reconnecting";

interface ConversationOutputClientOptions {
  onEnvelope: (envelope: OutputEnvelope) => void;
  onConnectionState: (state: OutputConnectionState) => void;
  onLagged?: (skipped: number) => void;
}

export class ConversationOutputClient {
  private source: EventSource | null = null;
  private sessionId: string | null = null;
  private lastSequence = 0;
  private opened = false;

  constructor(private readonly options: ConversationOutputClientOptions) {}

  connect(sessionId: string): Promise<boolean> {
    if (this.source && this.sessionId === sessionId) {
      return Promise.resolve(this.opened);
    }
    this.disconnect();
    this.sessionId = sessionId;
    this.lastSequence = 0;
    this.options.onConnectionState("connecting");

    const source = new EventSource(conversationOutputEventsUrl(sessionId));
    this.source = source;
    return new Promise((resolve) => {
      let settled = false;
      const settle = (connected: boolean) => {
        if (settled) return;
        settled = true;
        resolve(connected);
      };
      const timeout = window.setTimeout(() => settle(false), 2_000);

      source.onopen = () => {
        this.opened = true;
        window.clearTimeout(timeout);
        this.options.onConnectionState("connected");
        settle(true);
      };
      source.onerror = () => {
        this.opened = false;
        this.options.onConnectionState("reconnecting");
        settle(false);
      };
      source.addEventListener("output", (event) => {
        if (!(event instanceof MessageEvent)) return;
        try {
          const envelope: unknown = JSON.parse(String(event.data));
          if (
            !isOutputEnvelope(envelope) ||
            envelope.session_id !== this.sessionId ||
            envelope.sequence <= this.lastSequence
          ) {
            return;
          }
          this.lastSequence = envelope.sequence;
          this.options.onEnvelope(envelope);
        } catch {
          // A malformed external event is isolated from the rest of the output stream.
        }
      });
      source.addEventListener("lagged", (event) => {
        if (!(event instanceof MessageEvent)) return;
        const skipped = Number(event.data);
        if (Number.isFinite(skipped) && skipped > 0) this.options.onLagged?.(skipped);
      });
    });
  }

  disconnect() {
    this.source?.close();
    this.source = null;
    this.sessionId = null;
    this.lastSequence = 0;
    this.opened = false;
    this.options.onConnectionState("disconnected");
  }
}
