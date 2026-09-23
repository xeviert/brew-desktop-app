/**
 * Small display helpers shared by more than one component.
 *
 * `age` lived in `Sources.svelte` until Phase 2g gave the footer the same job. A
 * second copy would have drifted the moment one of them learned about seconds.
 */

/** Epoch seconds → "just now" / "12 min ago" / "3 h ago" / "5 d ago". */
export function age(ts: number | null | undefined): string {
	if (ts === null || ts === undefined) return "never";
	const mins = Math.round((Date.now() / 1000 - ts) / 60);
	if (mins < 1) return "just now";
	if (mins < 60) return `${mins} min ago`;
	const hours = Math.round(mins / 60);
	return hours < 48 ? `${hours} h ago` : `${Math.round(hours / 24)} d ago`;
}

/** Epoch seconds → "in 47 s" / "in 12 min". Clamped at zero: a deadline in the past
 * means the poll is starting, not that it is overdue by three seconds. */
export function until(ts: number | null | undefined): string {
	if (ts === null || ts === undefined) return "—";
	const secs = Math.max(0, Math.round(ts - Date.now() / 1000));
	if (secs < 90) return `in ${secs} s`;
	return `in ${Math.round(secs / 60)} min`;
}
