import { describe, expect, it } from "vitest";
import type { Packaging, PackagingLine } from "./api";
import { impliedVolume, lineText, lossText, packagingForm, toPackagingInput } from "./packaging";

function line(over: Partial<PackagingLine> = {}): PackagingLine {
	return {
		id: 1,
		batch_id: 1,
		event_id: 1,
		at: 1_700_000_000,
		format: "keg",
		container_size_l: 19,
		container_count: 1,
		volume_l: 19,
		notes: null,
		...over,
	};
}

function packaging(over: Partial<Packaging> = {}): Packaging {
	return {
		batch_id: 1,
		lines: [line()],
		pitched_at: 1_700_000_000,
		volume_l: 20,
		packaged_l: 17.3,
		loss_l: 2.7,
		cost: null,
		cost_error: null,
		...over,
	};
}

describe("toPackagingInput", () => {
	it("reads litres with a comma, and an empty volume as size × count", () => {
		const form = { ...packagingForm(null), format: "bottle" as const, size: "0,33", count: "24" };
		expect(toPackagingInput(form)).toEqual({
			ok: true,
			value: { format: "bottle", container_size_l: 0.33, container_count: 24, volume_l: null, notes: null },
		});
		expect(impliedVolume(form)).toBe(7.92);
	});

	it("refuses a part container and names unreadable fields", () => {
		const form = packagingForm(null);
		expect(toPackagingInput({ ...form, count: "1.5" }).ok).toBe(false);
		expect(toPackagingInput({ ...form, volume: "lots" })).toEqual({ ok: false, error: "Not readable: volume" });
	});

	it("round-trips a stored line", () => {
		const stored = line({ format: "can", container_size_l: 0.44, container_count: 12, volume_l: 5.2, notes: "x" });
		const back = toPackagingInput(packagingForm(stored));
		expect(back).toEqual({
			ok: true,
			value: { format: "can", container_size_l: 0.44, container_count: 12, volume_l: 5.2, notes: "x" },
		});
	});
});

describe("lineText", () => {
	it("names the containers, or the volume when they are not known", () => {
		expect(lineText(line())).toBe("Keg · 1 × 19 L");
		expect(lineText(line({ format: "bottle", container_size_l: 0.33, container_count: 24 }))).toBe(
			"Bottle · 24 × 330 mL",
		);
		expect(lineText(line({ format: "other", container_size_l: null, container_count: null, volume_l: 4.5 }))).toBe(
			"Other · 4.50 L",
		);
	});
});

describe("lossText", () => {
	it("is the loss and its share of the batch", () => {
		expect(lossText(packaging())).toBe("2.70 L lost (13.5 %)");
	});

	it("says when more was packaged than brewed, or nothing was lost", () => {
		expect(lossText(packaging({ loss_l: -0.5 }))).toBe("500 mL more than the batch volume");
		expect(lossText(packaging({ loss_l: 0 }))).toBe("none lost");
	});

	it("has nothing to say with no volume or no lines", () => {
		expect(lossText(packaging({ volume_l: null, loss_l: null }))).toBeNull();
		expect(lossText(packaging({ lines: [], loss_l: null }))).toBeNull();
	});
});
