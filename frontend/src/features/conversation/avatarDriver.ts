import type { OutputEnvelope } from "./outputProtocol";

export interface AvatarCue {
  emotion?: string;
  intensity?: number;
  motion?: string;
}

export interface AvatarDriver {
  applyCue(cue: AvatarCue, envelope: OutputEnvelope): void;
  reset?(): void;
  dispose?(): void;
}

export class AvatarDriverHub {
  private readonly drivers = new Set<AvatarDriver>();

  register(driver: AvatarDriver) {
    this.drivers.add(driver);
    return () => {
      this.drivers.delete(driver);
      driver.dispose?.();
    };
  }

  handle(envelope: OutputEnvelope): AvatarCue | null {
    if (envelope.output.type !== "avatar_cue") return null;
    const cue: AvatarCue = {
      emotion: normalizeText(envelope.output.emotion),
      intensity: normalizeIntensity(envelope.output.intensity),
      motion: normalizeText(envelope.output.motion)
    };
    for (const driver of this.drivers) driver.applyCue(cue, envelope);
    return cue;
  }

  reset() {
    for (const driver of this.drivers) driver.reset?.();
  }

  dispose() {
    for (const driver of this.drivers) driver.dispose?.();
    this.drivers.clear();
  }
}

function normalizeText(value: string | undefined) {
  const normalized = value?.trim();
  return normalized ? normalized : undefined;
}

function normalizeIntensity(value: number | undefined) {
  if (value === undefined || !Number.isFinite(value)) return undefined;
  return Math.max(0, Math.min(1, value));
}
