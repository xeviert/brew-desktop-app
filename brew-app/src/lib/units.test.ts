import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
	formatDuration,
	formatGravity,
	formatMass,
	formatMoney,
	formatTemp,
	formatTimestamp,
	formatVolume,
} from "./units";

// Intl separates a currency code from the amount with a no-break space.
const plain = (s: string) => s.replace(/ /g, " ");

beforeEach(() => vi.stubGlobal("navigator", { language: "en-GB" }));
afterEach(() => vi.unstubAllGlobals());

describe("formatTimestamp", () => {
	it("takes epoch seconds, not milliseconds", () => {
		const utc = { timeZone: "UTC", dateStyle: "medium", timeStyle: "short" } as const;
		expect(formatTimestamp(86_400, utc)).toBe("2 Jan 1970, 00:00");
	});
});

describe("formatDuration", () => {
	it.each([
		[0, "0m"],
		[59, "0m"],
		[60, "1m"],
		[3_599, "59m"],
		[3_600, "1h 0m"],
		[5_400, "1h 30m"],
		[86_399, "23h 59m"],
		[86_400, "1d 0h"],
		[97_200, "1d 3h"],
		[-5_400, "-1h 30m"],
	])("%i s is %s", (seconds, expected) => {
		expect(formatDuration(seconds)).toBe(expected);
	});
});

describe("formatMoney", () => {
	it("scales minor units by the currency's own exponent", () => {
		expect(formatMoney({ minor: 1050, currency: "EUR" })).toBe("€10.50");
		expect(formatMoney({ minor: 1234, currency: "JPY" })).toBe("JP¥1,234");
		expect(plain(formatMoney({ minor: 1500, currency: "KWD" }))).toBe("KWD 1.500");
	});

	it("renders a well-formed code Intl has no symbol for", () => {
		expect(plain(formatMoney({ minor: 1050, currency: "XYZ" }))).toBe("XYZ 10.50");
	});

	it("falls back rather than throwing on a code Intl rejects", () => {
		expect(formatMoney({ minor: 1050, currency: "E1R" })).toBe("10.50 E1R");
	});
});

describe("formatTemp", () => {
	it("converts only for display", () => {
		expect(formatTemp(18.44)).toBe("18.4 °C");
		expect(formatTemp(20, "F")).toBe("68.0 °F");
		expect(formatTemp(-40, "F", 0)).toBe("-40 °F");
	});
});

describe("formatGravity", () => {
	it("always shows four decimals", () => {
		expect(formatGravity(1)).toBe("1.0000");
		expect(formatGravity(1.05)).toBe("1.0500");
		expect(formatGravity(0.99812)).toBe("0.9981");
	});
});

describe("formatVolume", () => {
	it("switches to millilitres below a litre", () => {
		expect(formatVolume(0.25)).toBe("250 mL");
		expect(formatVolume(0.9996)).toBe("1000 mL");
		expect(formatVolume(1)).toBe("1.00 L");
		expect(formatVolume(23.456, 1)).toBe("23.5 L");
	});
});

describe("formatMass", () => {
	it("switches to kilograms at a thousand grams", () => {
		expect(formatMass(999)).toBe("999.0 g");
		expect(formatMass(1000)).toBe("1.0 kg");
		expect(formatMass(2500, 2)).toBe("2.50 kg");
	});
});
