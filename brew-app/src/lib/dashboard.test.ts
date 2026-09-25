import { describe, expect, it } from "vitest";
import { estimateText, formatGravityIn, gravitySource, span, stabilityText } from "./dashboard";

describe("formatGravityIn", () => {
	it("keeps SG bare and labels the degree scales", () => {
		expect(formatGravityIn(1.0374, "SG")).toBe("1.0374");
		expect(formatGravityIn(9.312, "°P")).toBe("9.3 °P");
	});
});

describe("span", () => {
	it("is hours under two days and days after", () => {
		expect(span(31 * 3_600 + 59)).toBe("31 h");
		expect(span(47 * 3_600)).toBe("47 h");
		expect(span(50 * 3_600)).toBe("2 d");
		expect(span(-5)).toBe("0 h");
	});
});

describe("stabilityText", () => {
	const now = 1_000_000;
	it("counts from since, not from now", () => {
		expect(stabilityText({ state: "stable", since: now - 31 * 3_600, fg_unchecked: false }, now)).toBe(
			"Stable for 31 h",
		);
	});
	it("says when there was no FG to check against", () => {
		expect(stabilityText({ state: "stable", since: now, fg_unchecked: true }, now)).toContain("FG unchecked");
	});
	it("tells a stall from a finish", () => {
		expect(stabilityText({ state: "stalled", since: now - 3 * 86_400 }, now)).toBe(
			"Stalled for 3 d, above expected FG",
		);
	});
	it("names the lag phase", () => {
		expect(stabilityText({ state: "insufficient", reason: "not_started" }, now)).toBe("Not started");
	});
});

describe("estimateText", () => {
	it("labels the formula", () => {
		const text = estimateText({ state: "estimated", abv_alternate: 0.0612, apparent_attenuation: 1.02 });
		expect(text).toBe("Est. ABV 6.1 % (alternate) · 102 % apparent attenuation");
	});
	it("says why there is none", () => {
		expect(estimateText({ state: "no_og" })).toBe("No ABV: no OG");
		expect(estimateText({ state: "not_started" })).toBe("No ABV: still at OG");
	});
});

describe("gravitySource", () => {
	const g = { ts: 0, sg: 1.01, source_kind: "manual", label: "manual" };
	it("prefers the instrument", () => {
		expect(gravitySource({ ...g, smoothed: false, instrument: "hydrometer" })).toBe("hydrometer");
	});
	it("says a sensor value is smoothed", () => {
		expect(gravitySource({ ...g, label: "ispindel", smoothed: true, instrument: null })).toBe(
			"ispindel, smoothed",
		);
	});
});
