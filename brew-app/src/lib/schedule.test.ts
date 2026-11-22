import { describe, expect, it } from "vitest";
import type { Reminder, ScheduleStep } from "./api";
import { currentIndex, progress, reminderDay, reminderState, stepStart, targetsText, type Now } from "./schedule";

function step(over: Partial<ScheduleStep> = {}): ScheduleStep {
	return {
		id: 1,
		batch_id: 1,
		position: 1,
		name: "Primary",
		temp_min_c: null,
		temp_max_c: null,
		target_sg: null,
		min_hours: null,
		note: null,
		done_event_id: null,
		done_at: null,
		target: null,
		...over,
	};
}

const PITCH = 1_700_000_000;
const HOUR = 3_600;

function now(over: Partial<Now> = {}): Now {
	return { ts: PITCH + 80 * HOUR, temp_c: 10, sg: 1.02, gravity: 1.02, unit: "SG", temp_display: "C", ...over };
}

describe("currentIndex / stepStart", () => {
	it("is the first step not done, started when the one before was done", () => {
		const steps = [step({ done_at: PITCH + 72 * HOUR }), step({ id: 2 })];
		expect(currentIndex(steps)).toBe(1);
		expect(stepStart(steps, 0, PITCH)).toBe(PITCH);
		expect(stepStart(steps, 1, PITCH)).toBe(PITCH + 72 * HOUR);
	});

	it("is null once every step is done", () => {
		expect(currentIndex([step({ done_at: PITCH })])).toBeNull();
	});
});

describe("progress", () => {
	const primary = step({ temp_min_c: 9, temp_max_c: 12, target_sg: 1.023, target: 1.023, min_hours: 72 });

	it("is ready when every target is met", () => {
		const p = progress(primary, PITCH, now());
		expect(p.checks.map((c) => c.met)).toEqual([true, true, true]);
		expect(p.ready).toBe(true);
	});

	it("waits on the slowest target", () => {
		const p = progress(primary, PITCH, now({ sg: 1.03, gravity: 1.03 }));
		expect(p.checks[1]).toMatchObject({ label: "Gravity", met: false, detail: "1.0300 → 1.0230" });
		expect(p.ready).toBe(false);
		expect(progress(primary, PITCH, now({ ts: PITCH + HOUR })).checks[2]).toMatchObject({
			met: false,
			detail: "1 h of 3 d min",
		});
	});

	it("doesn't judge without a reading", () => {
		const p = progress(primary, PITCH, now({ temp_c: null }));
		expect(p.checks[0].met).toBeNull();
		expect(p.ready).toBe(false);
	});

	it("a step with no targets is never ready by itself", () => {
		expect(progress(step(), PITCH, now()).ready).toBe(false);
	});
});

describe("targetsText", () => {
	it("says every target set", () => {
		const s = step({ temp_min_c: 0, temp_max_c: 2, target: 2.5, target_sg: 1.01, min_hours: 336 });
		expect(targetsText(s, "°P", "C")).toBe("0.0 °C – 2.0 °C · to 2.5 °P · 14 d min");
		expect(targetsText(step(), "SG", "C")).toBe("");
	});
});

describe("reminders", () => {
	const reminder = (over: Partial<Reminder> = {}): Reminder => ({
		id: 1,
		batch_id: 1,
		after_hours: 240,
		text: "Taste",
		done_event_id: null,
		done_at: null,
		...over,
	});
	const pitch = 1_790_000_000;

	it("names the day", () => {
		expect(reminderDay(reminder())).toBe("day 10");
		expect(reminderDay(reminder({ after_hours: 36 }))).toBe("day 1.5");
	});
	it("is upcoming, due, then overdue past the grace", () => {
		const at = (h: number) => reminderState(reminder(), pitch, pitch + h * 3_600, 12).state;
		expect([at(239), at(240), at(251), at(252)]).toEqual(["upcoming", "due", "due", "overdue"]);
		expect(reminderState(reminder(), pitch, 0, 12).due).toBe(pitch + 240 * 3_600);
	});
	it("waits for the pitch and stops at done", () => {
		expect(reminderState(reminder(), null, pitch, 12)).toEqual({ state: "upcoming", due: null });
		expect(reminderState(reminder({ done_at: pitch }), pitch, pitch + 1e7, 12).state).toBe("done");
	});
});
