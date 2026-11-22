import { describe, expect, it } from "vitest";
import { adjustForm, adjustmentText, remainingText, toAdjustInput } from "./inventory";

describe("remainingText", () => {
	it("is what is left, empty, or over", () => {
		expect(remainingText({ value: 0.75, unit: "kg" })).toEqual({ text: "0.75 kg left", over: false, empty: false });
		expect(remainingText({ value: 1e-9, unit: "kg" })).toEqual({ text: "empty", over: false, empty: true });
		expect(remainingText({ value: -0.2, unit: "kg" })).toEqual({ text: "over by 0.2 kg", over: true, empty: true });
	});
});

describe("toAdjustInput", () => {
	it("takes stock out with a reason", () => {
		const form = { ...adjustForm("g"), qty: "200", reason: " spilt " };
		expect(toAdjustInput(form)).toEqual({
			ok: true,
			value: { mode: "remove", adjustment: { entry: { value: 200, unit: "g" }, reason: "spilt" } },
		});
		expect(toAdjustInput({ ...form, reason: "" }).ok).toBe(false);
	});

	it("counts what is left, zero included, with an optional reason", () => {
		const form = { ...adjustForm("kg"), mode: "count" as const, qty: "0" };
		expect(toAdjustInput(form)).toEqual({
			ok: true,
			value: { mode: "count", counted: { value: 0, unit: "kg" }, reason: null },
		});
		expect(toAdjustInput({ ...form, qty: "-1" }).ok).toBe(false);
	});

	it("names an unreadable or missing quantity", () => {
		expect(toAdjustInput({ ...adjustForm("g"), qty: "lots" })).toEqual({ ok: false, error: "Not readable: quantity" });
		expect(toAdjustInput(adjustForm("g"))).toEqual({ ok: false, error: "Give a quantity" });
	});
});

it("signs an adjustment by which way the stock moved", () => {
	expect(adjustmentText({ value: 200, unit: "g" })).toBe("−200 g");
	expect(adjustmentText({ value: -50, unit: "g" })).toBe("+50 g");
});
