import { describe, expect, it } from "vitest";
import type { AlertRow } from "./api";
import { alertState, alertTitle, isUrgent, openByBatch, ruleText, syncText } from "./alerts";

function alert(over: Partial<AlertRow> = {}): AlertRow {
	return {
		id: 1,
		batch_id: 7,
		code: "SELTZER-007",
		rule: "temp_high",
		device: null,
		raised_at: 1_790_000_000,
		detail: "20.5 °C, above 20.0 °C for 10 min",
		cleared_at: null,
		cleared_note: null,
		acked_at: null,
		snoozed_until: null,
		notified_at: null,
		...over,
	};
}

describe("alertTitle", () => {
	it("names the rule", () => {
		expect(alertTitle(alert())).toBe("Too warm");
		expect(ruleText("sugar_break")).toBe("⅓ sugar break");
	});
	it("names the device for a silent one", () => {
		expect(alertTitle(alert({ rule: "silent", device: "ispindel-1" }))).toBe("ispindel-1 silent");
	});
});

describe("alertState", () => {
	const now = 1_790_001_000;
	it("goes from unsent to sent", () => {
		expect(alertState(alert(), now)).toBe("not sent yet");
		expect(alertState(alert({ notified_at: now - 10 }), now)).toBe("sent to phone");
	});
	it("puts an ack over a snooze, and a lapsed snooze is just sent", () => {
		expect(alertState(alert({ acked_at: now, snoozed_until: now + 60 }), now)).toBe("acknowledged");
		expect(alertState(alert({ snoozed_until: now + 60 }), now)).toMatch(/^snoozed until /);
		expect(alertState(alert({ notified_at: now - 10, snoozed_until: now - 1 }), now)).toBe("sent to phone");
	});
	it("says why brewd cleared it when the rule didn't", () => {
		expect(alertState(alert({ cleared_at: now, cleared_note: "batch no longer watched" }), now)).toMatch(
			/— batch no longer watched$/,
		);
	});
});

describe("isUrgent", () => {
	it("follows brewd's push priority", () => {
		expect(isUrgent(alert({ rule: "stalled" }))).toBe(true);
		expect(isUrgent(alert({ rule: "finished" }))).toBe(false);
	});
});

describe("openByBatch", () => {
	it("leaves out the cleared", () => {
		const map = openByBatch([alert(), alert({ id: 2, cleared_at: 5 }), alert({ id: 3, batch_id: 8 })]);
		expect(map.get(7)?.map((a) => a.id)).toEqual([1]);
		expect(map.get(8)?.length).toBe(1);
	});
});

describe("syncText", () => {
	const base = { at: 1, report: null, error_code: null, error: null };
	it("separates unconfigured from failing", () => {
		expect(syncText({ ...base, at: null })).toBe("not checked yet");
		expect(syncText({ ...base, error_code: "brewd.unconfigured", error: "no brewd address is set" })).toBe(
			"brewd URL not set",
		);
		expect(syncText({ ...base, error_code: "alert.write_refused", error: "brewd refused the write: x" })).toBe(
			"brewd refused the write: x",
		);
	});
	it("says when no token keeps brewd in the dark", () => {
		expect(syncText({ ...base, report: { watches: "no_token", watched: 2 } })).toMatch(/^no token/);
		expect(syncText({ ...base, report: { watches: "unchanged", watched: 1 } })).toBe("brewd watching 1 batch");
	});
});
