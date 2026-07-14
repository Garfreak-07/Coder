import type { OutputEnvelope, OutputStreamBehavior } from "./outputProtocol";

interface SpeechIntent {
  id: string;
  streamId: string;
  priority: number;
  behavior: OutputStreamBehavior;
  createdAt: number;
  tokens: Map<number, string>;
  ended: boolean;
  cancelled: boolean;
}

export class BrowserSpeechOutput {
  private readonly intents = new Map<string, SpeechIntent>();
  private readonly pending: SpeechIntent[] = [];
  private active: SpeechIntent | null = null;
  private enabled = false;

  get available() {
    return typeof window !== "undefined" && "speechSynthesis" in window && "SpeechSynthesisUtterance" in window;
  }

  setEnabled(enabled: boolean) {
    this.enabled = enabled && this.available;
    if (!this.enabled) this.stopAll();
  }

  handle(envelope: OutputEnvelope) {
    const event = envelope.output;
    switch (event.type) {
      case "speech_intent_started":
        this.open({
          id: event.intent_id,
          streamId: event.stream_id,
          priority: event.priority,
          behavior: event.behavior,
          createdAt: Date.now(),
          tokens: new Map(),
          ended: false,
          cancelled: false
        });
        break;
      case "speech_intent_token": {
        const intent = this.intents.get(event.intent_id);
        if (!intent || intent.cancelled || event.kind !== "literal" || !event.value) return;
        intent.tokens.set(event.sequence, event.value);
        break;
      }
      case "speech_intent_ended": {
        const intent = this.intents.get(event.intent_id);
        if (!intent || intent.cancelled) return;
        intent.ended = true;
        if (this.active?.id === intent.id) this.playActive();
        break;
      }
      case "speech_intent_cancelled":
        this.cancelIntent(event.intent_id);
        break;
    }
  }

  stopAll() {
    this.intents.clear();
    this.pending.length = 0;
    this.active = null;
    if (this.available) window.speechSynthesis.cancel();
  }

  private open(intent: SpeechIntent) {
    if (!this.enabled || this.intents.has(intent.id)) return;
    this.intents.set(intent.id, intent);
    if (!this.active) {
      this.active = intent;
      return;
    }
    if (intent.behavior === "replace") {
      this.cancelIntent(this.active.id, false);
      this.active = intent;
      return;
    }
    if (intent.behavior === "interrupt" && intent.priority >= this.active.priority) {
      this.cancelIntent(this.active.id, false);
      this.active = intent;
      return;
    }
    this.pending.push(intent);
  }

  private playActive() {
    const intent = this.active;
    if (!this.enabled || !intent || !intent.ended || intent.cancelled) return;
    const text = [...intent.tokens.entries()]
      .sort(([left], [right]) => left - right)
      .map(([, value]) => value)
      .join("")
      .trim();
    if (!text) {
      this.finishIntent(intent.id);
      return;
    }
    const utterance = new SpeechSynthesisUtterance(text);
    utterance.lang = navigator.language;
    utterance.onend = () => this.finishIntent(intent.id);
    utterance.onerror = () => this.finishIntent(intent.id);
    window.speechSynthesis.speak(utterance);
  }

  private cancelIntent(intentId: string, activateNext = true) {
    const intent = this.intents.get(intentId);
    if (!intent) return;
    intent.cancelled = true;
    this.intents.delete(intentId);
    const pendingIndex = this.pending.findIndex((item) => item.id === intentId);
    if (pendingIndex >= 0) this.pending.splice(pendingIndex, 1);
    if (this.active?.id !== intentId) return;
    if (this.available) window.speechSynthesis.cancel();
    this.active = null;
    if (activateNext) this.activateNext();
  }

  private finishIntent(intentId: string) {
    this.intents.delete(intentId);
    if (this.active?.id === intentId) this.active = null;
    this.activateNext();
  }

  private activateNext() {
    if (this.active || this.pending.length === 0) return;
    this.pending.sort((left, right) => right.priority - left.priority || left.createdAt - right.createdAt);
    this.active = this.pending.shift() ?? null;
    this.playActive();
  }
}
