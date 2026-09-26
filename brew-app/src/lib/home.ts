/**
 * Phase 4f. The home screen's wording, pure so it is tested without a component.
 * Staleness and the guesses are computed in Rust; this only puts them beside the sync
 * status and says them.
 */
import type { Freshness, SilenceGuess, SourceStatus } from "$lib/api";

export type Silence = "fresh" | "unreachable" | "sync_error" | "silent";

/**
 * Why a stale stream is stale. A stream can only be called silent when its source is
 * syncing cleanly: with `brewd` unreachable or the sync failing, the app never saw the
 * readings, so it cannot tell whether the sensor sent any. Branches on the phase, never
 * on message text — `last_error_code` can't tell a refused connection from a bad
 * response, and `phase` already has.
 */
export function silence(
  stale: boolean,
  sync: SourceStatus | undefined,
): Silence {
  if (!stale) return "fresh";
  if (sync?.phase === "unreachable") return "unreachable";
  if (sync?.phase === "error") return "sync_error";
  return "silent";
}

export function silenceText(
  s: Silence,
  guess: SilenceGuess | null = null,
): string {
  switch (s) {
    case "fresh":
      return "";
    case "unreachable":
      return "brewd unreachable — can't tell if the sensor is silent";
    case "sync_error":
      return "sync failing — can't tell if the sensor is silent";
    case "silent":
      return guess ? `sensor silent · ${guessText(guess)}` : "sensor silent";
  }
}

/** Labelled as a guess: a dead device and a finished ferment look the same. */
export function guessText(g: SilenceGuess): string {
  return g === "battery" ? "battery likely (guess)" : "signal likely (guess)";
}

const METRIC_NAMES: Record<string, string> = {
  sg: "gravity",
  temp_c: "temp",
  gravity_temp_c: "iSpindle temp",
  setpoint_c: "setpoint",
  room_temp_c: "room temp",
  battery_v: "battery",
  rssi_dbm: "signal",
  ph: "pH",
};

export function metricName(metric: string): string {
  return METRIC_NAMES[metric] ?? metric;
}

export interface QuietSource {
  source_id: number;
  label: string;
  silence: Exclude<Silence, "fresh">;
  /** Newest reading among the quiet streams. */
  newest: number | null;
  /** Which streams, when only some of the source's are quiet; `null` when all are. */
  metrics: string[] | null;
}

/** A batch's quiet streams, one line per source: a silent device goes quiet on every
 * stream at once, and seven lines saying so is noise. */
export function quietSources(
  streams: Freshness[],
  syncs: Record<number, SourceStatus>,
): QuietSource[] {
  const bySource = new Map<number, Freshness[]>();
  for (const s of streams)
    bySource.set(s.source_id, [...(bySource.get(s.source_id) ?? []), s]);
  const out: QuietSource[] = [];
  for (const [source_id, all] of bySource) {
    const quiet = all.filter((s) => s.stale);
    if (quiet.length === 0) continue;
    const q = silence(true, syncs[source_id]) as QuietSource["silence"];
    const times = quiet
      .map((s) => s.newest)
      .filter((t): t is number => t !== null);
    out.push({
      source_id,
      label: quiet[0].label,
      silence: q,
      newest: times.length > 0 ? Math.max(...times) : null,
      metrics:
        quiet.length === all.length
          ? null
          : quiet.map((s) => metricName(s.metric)),
    });
  }
  return out.sort((a, b) => a.label.localeCompare(b.label));
}

export function formatVolts(v: number): string {
  return `${v.toFixed(2)} V`;
}

export function formatRssi(dbm: number): string {
  return `${Math.round(dbm)} dBm`;
}

/** `points` scaled into a `w` × `h` box, newest on the right and highest at the top. A
 * flat series sits in the middle rather than along an edge. */
export function sparklinePoints(
  points: [number, number][],
  w: number,
  h: number,
): [number, number][] {
  if (points.length === 0) return [];
  const ts = points.map((p) => p[0]);
  const vs = points.map((p) => p[1]);
  const [t0, t1] = [Math.min(...ts), Math.max(...ts)];
  const [v0, v1] = [Math.min(...vs), Math.max(...vs)];
  const x = (t: number) => (t1 === t0 ? w / 2 : ((t - t0) / (t1 - t0)) * w);
  const y = (v: number) => (v1 === v0 ? h / 2 : h - ((v - v0) / (v1 - v0)) * h);
  return points.map(([t, v]) => [x(t), y(v)]);
}

/** An SVG path through `sparklinePoints`. Empty with nothing to draw. */
export function sparklinePath(
  points: [number, number][],
  w: number,
  h: number,
): string {
  return sparklinePoints(points, w, h)
    .map(([x, y], i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(1)}`)
    .join(" ");
}
