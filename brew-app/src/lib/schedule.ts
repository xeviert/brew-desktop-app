/**
 * The fermentation schedule's reading of a batch: which step it's on, and how far that
 * step is from its targets. Pure, so it is tested without a component.
 */
import type { Reminder, ScheduleStep } from "$lib/api";
import { formatGravityIn, span } from "$lib/dashboard";
import { formatTemp, type TempDisplay } from "$lib/units";

/** Index of the first step not done; `null` once every one is. */
export function currentIndex(steps: ScheduleStep[]): number | null {
	const i = steps.findIndex((s) => s.done_at === null);
	return i < 0 ? null : i;
}

/** The step before done, the first at the pitch. `null` until then. */
export function stepStart(steps: ScheduleStep[], i: number, pitchedAt: number | null): number | null {
	return i === 0 ? pitchedAt : (steps[i - 1]?.done_at ?? null);
}

export interface Check {
	label: string;
	/** What it is now, against the target. */
	detail: string;
	/** `null`: no reading to judge by. */
	met: boolean | null;
}

export interface Progress {
	checks: Check[];
	/** Every target met, and there is at least one. */
	ready: boolean;
}

export interface Now {
	ts: number;
	temp_c: number | null;
	/** SG, and the same gravity in `unit` for display. */
	sg: number | null;
	gravity: number | null;
	unit: string;
	temp_display: TempDisplay;
}

export function band(step: ScheduleStep, display: TempDisplay): string | null {
	const { temp_min_c: lo, temp_max_c: hi } = step;
	if (lo !== null && hi !== null) return `${formatTemp(lo, display)} – ${formatTemp(hi, display)}`;
	if (lo !== null) return `≥ ${formatTemp(lo, display)}`;
	if (hi !== null) return `≤ ${formatTemp(hi, display)}`;
	return null;
}

/** A step's targets in one line, for the steps not yet started. */
export function targetsText(step: ScheduleStep, unit: string, display: TempDisplay): string {
	const parts: string[] = [];
	const temp = band(step, display);
	if (temp) parts.push(temp);
	if (step.target !== null) parts.push(`to ${formatGravityIn(step.target, unit)}`);
	if (step.min_hours !== null) parts.push(`${span(step.min_hours * 3_600)} min`);
	return parts.join(" · ");
}

export function progress(step: ScheduleStep, start: number | null, now: Now): Progress {
	const checks: Check[] = [];
	const temp = band(step, now.temp_display);
	if (temp) {
		const t = now.temp_c;
		checks.push({
			label: "Temp",
			detail: t === null ? `no reading · want ${temp}` : `${formatTemp(t, now.temp_display)} · want ${temp}`,
			met:
				t === null
					? null
					: (step.temp_min_c === null || t >= step.temp_min_c) && (step.temp_max_c === null || t <= step.temp_max_c),
		});
	}
	if (step.target_sg !== null && step.target !== null) {
		const want = formatGravityIn(step.target, now.unit);
		checks.push({
			label: "Gravity",
			detail:
				now.sg === null || now.gravity === null
					? `no reading · want ${want}`
					: `${formatGravityIn(now.gravity, now.unit)} → ${want}`,
			met: now.sg === null ? null : now.sg <= step.target_sg,
		});
	}
	if (step.min_hours !== null) {
		const want = step.min_hours * 3_600;
		checks.push({
			label: "Time",
			detail: start === null ? `not started · ${span(want)} min` : `${span(now.ts - start)} of ${span(want)} min`,
			met: start === null ? false : now.ts - start >= want,
		});
	}
	return { checks, ready: checks.length > 0 && checks.every((c) => c.met === true) };
}

// ------------------------------------------------------------- reminders

/** "day 10", "day 1.5". */
export function reminderDay(r: Reminder): string {
	return `day ${Number((r.after_hours / 24).toFixed(2))}`;
}

export type ReminderState = "done" | "overdue" | "due" | "upcoming";

/** Where a reminder stands at `now`, and the time it falls due (`null` before the
 * pitch). Overdue past `graceHours`, as `brewd` judges it. */
export function reminderState(
	r: Reminder,
	pitchedAt: number | null,
	now: number,
	graceHours: number,
): { state: ReminderState; due: number | null } {
	const due = pitchedAt === null ? null : pitchedAt + r.after_hours * 3_600;
	if (r.done_at !== null) return { state: "done", due };
	if (due === null || now < due) return { state: "upcoming", due };
	return { state: now - due >= graceHours * 3_600 ? "overdue" : "due", due };
}
