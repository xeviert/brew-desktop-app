/**
 * Phase 4f. The home screen's wording, pure so it is tested without a component.
 * Staleness and the guesses are computed in Rust; this only puts them beside the sync
 * status and says them.
 */
import type { SilenceGuess, SourceStatus } from "$lib/api";

export type Silence = "fresh" | "unreachable" | "sync_error" | "silent";

/**
 * Why a stale stream is stale. A stream can only be called silent when its source is
 * syncing cleanly: with `brewd` unreachable or the sync failing, the app never saw the
 * readings, so it cannot tell whether the sensor sent any. Branches on the phase, never
 * on message text — `last_error_code` can't tell a refused connection from a bad
 * response, and `phase` already has.
 */
export function silence(stale: boolean, sync: SourceStatus | undefined): Silence {
	if (!stale) return "fresh";
	if (sync?.phase === "unreachable") return "unreachable";
	if (sync?.phase === "error") return "sync_error";
	return "silent";
}

export function silenceText(s: Silence, guess: SilenceGuess | null = null): string {
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

export function formatVolts(v: number): string {
	return `${v.toFixed(2)} V`;
}

export function formatRssi(dbm: number): string {
	return `${Math.round(dbm)} dBm`;
}

/** An SVG path through `points` scaled to `w` × `h`. Empty with nothing to draw; a flat
 * series is drawn through the middle rather than along an edge. */
export function sparklinePath(points: [number, number][], w: number, h: number): string {
	if (points.length === 0) return "";
	const ts = points.map((p) => p[0]);
	const vs = points.map((p) => p[1]);
	const [t0, t1] = [Math.min(...ts), Math.max(...ts)];
	const [v0, v1] = [Math.min(...vs), Math.max(...vs)];
	const x = (t: number) => (t1 === t0 ? w / 2 : ((t - t0) / (t1 - t0)) * w);
	const y = (v: number) => (v1 === v0 ? h / 2 : h - ((v - v0) / (v1 - v0)) * h);
	return points
		.map(([t, v], i) => `${i === 0 ? "M" : "L"}${x(t).toFixed(1)} ${y(v).toFixed(1)}`)
		.join(" ");
}
