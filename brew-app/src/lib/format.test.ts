import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { age, bytes, until } from "./format";

const NOW = 1_790_000_000;

beforeEach(() => {
	vi.useFakeTimers();
	vi.setSystemTime(NOW * 1000);
});
afterEach(() => vi.useRealTimers());

describe("age", () => {
	it.each([
		[null, "never"],
		[undefined, "never"],
		[NOW, "just now"],
		[NOW - 29, "just now"],
		[NOW - 30, "1 min ago"],
		[NOW - 59 * 60, "59 min ago"],
		[NOW - 60 * 60, "1 h ago"],
		[NOW - 47 * 3600, "47 h ago"],
		[NOW - 48 * 3600, "2 d ago"],
		// A clock that runs ahead of the Pi's must not read as "-3 min ago".
		[NOW + 180, "just now"],
	])("%s is %s", (ts, expected) => {
		expect(age(ts)).toBe(expected);
	});
});

describe("until", () => {
	it.each([
		[null, "—"],
		[undefined, "—"],
		[NOW + 47, "in 47 s"],
		[NOW + 89, "in 89 s"],
		[NOW + 90, "in 2 min"],
		[NOW + 12 * 60, "in 12 min"],
		[NOW - 3, "in 0 s"],
	])("%s is %s", (ts, expected) => {
		expect(until(ts)).toBe(expected);
	});
});

describe("bytes", () => {
	it.each([
		[0, "0 B"],
		[1023, "1023 B"],
		[1024, "1.0 KiB"],
		[14_540, "14.2 KiB"],
		[1024 * 1024, "1.0 MiB"],
		[3_250_586, "3.1 MiB"],
	])("%i is %s", (n, expected) => {
		expect(bytes(n)).toBe(expected);
	});
});
