import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Cleaning, VesselHygiene } from "./api";
import { cleaningLabel, hygieneText, pitchWarning } from "./cleaning";

const NOW = 1_700_000_000;
const DAY = 86_400;

function entry(over: Partial<Cleaning> = {}): Cleaning {
	return { id: 1, vessel_id: 1, cleaned_at: NOW - 3 * DAY, kind: "sanitize", chemical: null, notes: null, ...over };
}

function hygiene(over: Partial<VesselHygiene> = {}): VesselHygiene {
	return {
		vessel_id: 1,
		last_cleaned: null,
		last_sanitized: null,
		last_batch: null,
		needs_sanitizing: false,
		...over,
	};
}

const left = { id: 7, code: "SELTZER-017", pitched_at: NOW - 20 * DAY, in_vessel: false };

beforeEach(() => {
	vi.useFakeTimers();
	vi.setSystemTime(NOW * 1000);
});

afterEach(() => {
	vi.useRealTimers();
});

describe("hygieneText", () => {
	it("says what is in the vessel before anything else", () => {
		expect(hygieneText(hygiene({ last_batch: { ...left, in_vessel: true } }))).toEqual({
			text: "In use: SELTZER-017",
			warn: false,
		});
	});

	it("flags a vessel a batch left with no sanitise since", () => {
		const h = hygiene({
			last_batch: left,
			needs_sanitizing: true,
			last_sanitized: entry({ cleaned_at: NOW - 30 * DAY }),
		});
		expect(hygieneText(h)).toEqual({ text: "Not sanitised since SELTZER-017 (last sanitised 30 d ago)", warn: true });
	});

	it("is the last sanitise, else the last clean, else nothing", () => {
		expect(hygieneText(hygiene({ last_sanitized: entry() })).text).toBe("Last sanitised 3 d ago");
		expect(hygieneText(hygiene({ last_cleaned: entry({ kind: "clean" }) })).text).toBe("Last cleaned 3 d ago");
		expect(hygieneText(hygiene()).text).toBe("Nothing logged");
	});
});

describe("pitchWarning", () => {
	it("warns of a batch still in the vessel, or one that left it unsanitised", () => {
		expect(pitchWarning(hygiene({ last_batch: { ...left, in_vessel: true } }), "FV1")).toBe(
			"FV1 still holds SELTZER-017.",
		);
		expect(pitchWarning(hygiene({ last_batch: left, needs_sanitizing: true }), "FV1")).toBe(
			"FV1 has no sanitise logged since SELTZER-017 left it.",
		);
	});

	it("is quiet for a sanitised or never used vessel", () => {
		expect(pitchWarning(hygiene({ last_batch: left, last_sanitized: entry() }), "FV1")).toBeNull();
		expect(pitchWarning(hygiene(), "FV1")).toBeNull();
	});
});

it("labels every kind", () => {
	expect(cleaningLabel("clean_and_sanitize")).toBe("Cleaned and sanitised");
});
