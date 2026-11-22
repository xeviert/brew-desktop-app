/**
 * Phase 4e. The status card's wording, pure so it is tested without a component.
 * Every number is computed in Rust; this only says it.
 */
import type { DashboardGravity, Estimate, ExpectedFg, Stability } from "$lib/api";
import { formatGravity } from "$lib/units";

/** A gravity already in `unit`: SG as everywhere else, °P and °Bx to one place. */
export function formatGravityIn(value: number, unit: string): string {
	return unit === "SG" ? formatGravity(value) : `${value.toFixed(1)} ${unit}`;
}

/** Whole hours under two days, whole days after. */
export function span(seconds: number): string {
	const hours = Math.max(0, Math.floor(seconds / 3_600));
	return hours < 48 ? `${hours} h` : `${Math.floor(hours / 24)} d`;
}

export function stabilityText(s: Stability, now: number): string {
	switch (s.state) {
		case "active":
			return "Fermenting";
		case "stable":
			return `Stable for ${span(now - s.since)}${s.fg_unchecked ? " — FG unchecked" : ""}`;
		case "stalled":
			return `Stalled for ${span(now - s.since)}, above expected FG`;
		case "insufficient":
			switch (s.reason) {
				case "empty":
					return "No sensor gravity";
				case "not_started":
					return "Not started";
				default:
					return "Not enough readings to judge";
			}
	}
}

export function fgSourceText(fg: ExpectedFg): string {
	return fg.source === "recipe" ? "recipe target" : "yeast attenuation";
}

/** 3f's rule: the ABV always says which formula it is. */
export function estimateText(e: Estimate): string {
	switch (e.state) {
		case "estimated":
			return (
				`Est. ABV ${(e.abv_alternate * 100).toFixed(1)} % (alternate) · ` +
				`${(e.apparent_attenuation * 100).toFixed(0)} % apparent attenuation`
			);
		case "no_og":
			return "No ABV: no OG";
		case "no_gravity":
			return "No ABV: no gravity yet";
		case "not_started":
			return "No ABV: still at OG";
		case "og_too_low":
			return "No ABV: OG too close to water";
	}
}

/** What took it: the instrument when one was recorded, else the source's label. */
export function gravitySource(g: DashboardGravity): string {
	const what = g.instrument ?? g.label;
	return g.smoothed ? `${what}, smoothed` : what;
}
