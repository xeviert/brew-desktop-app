/**
 * Phase 7b. The cleaning log, pure; `VesselCleaning.svelte`, `Vessels.svelte` and the pitch
 * form own the calls. Whether a vessel needs sanitising is the backend's
 * (`cleaning::vessel_hygiene`); this file only words it.
 */
import type { CleaningKind, VesselHygiene } from "$lib/api";
import { age } from "$lib/format";

export const CLEANING_KINDS: { value: CleaningKind; label: string }[] = [
	{ value: "clean_and_sanitize", label: "Cleaned and sanitised" },
	{ value: "clean", label: "Cleaned" },
	{ value: "sanitize", label: "Sanitised" },
	{ value: "rinse", label: "Rinsed" },
];

export const cleaningLabel = (kind: CleaningKind): string =>
	CLEANING_KINDS.find((k) => k.value === kind)?.label ?? kind;

/** The vessel's state in a line, and whether it is worth flagging. */
export function hygieneText(h: VesselHygiene): { text: string; warn: boolean } {
	const batch = h.last_batch;
	if (batch?.in_vessel) return { text: `In use: ${batch.code}`, warn: false };
	const sanitised = h.last_sanitized ? `sanitised ${age(h.last_sanitized.cleaned_at)}` : null;
	if (h.needs_sanitizing && batch) {
		return { text: `Not sanitised since ${batch.code}${sanitised ? ` (last ${sanitised})` : ""}`, warn: true };
	}
	if (sanitised) return { text: `Last ${sanitised}`, warn: false };
	if (h.last_cleaned) return { text: `Last cleaned ${age(h.last_cleaned.cleaned_at)}`, warn: false };
	return { text: "Nothing logged", warn: false };
}

/** For the pitch form: a warning, never a refusal. `null` when there is nothing to say. */
export function pitchWarning(h: VesselHygiene, vessel: string): string | null {
	const batch = h.last_batch;
	if (batch?.in_vessel) return `${vessel} still holds ${batch.code}.`;
	if (h.needs_sanitizing && batch) return `${vessel} has no sanitise logged since ${batch.code} left it.`;
	return null;
}
