/**
 * Formatting only. The Rust `units` module owns storage and conversion; this file
 * turns canonical values into strings for display and never the other way round.
 *
 * Deliberately absent: gravity scale conversion. SG -> Plato is a polynomial fit
 * that lives in `crates/brewcore/src/calc`; a second copy here would drift.
 */

/** Matches `brewcore::units::Currency` — an ISO 4217 alphabetic code. */
export type Currency = string;

/** Matches `brewcore::units::Money`. `minor` is an integer, e.g. cents. */
export interface Money {
	minor: number;
	currency: Currency;
}

export type TempDisplay = "C" | "F";

/** The user's locale, resolved once. */
const locale = (): string => navigator.language || "en-GB";

/**
 * Epoch seconds (not milliseconds — the Rust side stores seconds) to local time.
 */
export function formatTimestamp(
	seconds: number,
	options: Intl.DateTimeFormatOptions = { dateStyle: "medium", timeStyle: "short" },
): string {
	return new Intl.DateTimeFormat(locale(), options).format(seconds * 1000);
}

export function formatDuration(seconds: number): string {
	const total = Math.abs(Math.round(seconds));
	const hours = Math.floor(total / 3600);
	const minutes = Math.floor((total % 3600) / 60);
	const sign = seconds < 0 ? "-" : "";
	if (hours >= 24) {
		const days = Math.floor(hours / 24);
		return `${sign}${days}d ${hours % 24}h`;
	}
	return hours > 0 ? `${sign}${hours}h ${minutes}m` : `${sign}${minutes}m`;
}

/**
 * `Intl.NumberFormat` already knows each currency's symbol placement and digit
 * count, so the minor-unit exponent table is not duplicated here — it is read
 * back off the formatter and used to scale the integer.
 *
 * The Rust side accepts any three ASCII letters as a currency code rather than
 * shipping the ISO register, so a typo reaches us as a code `Intl` throws a
 * `RangeError` on. Fall back to "<amount> <code>" — a stored amount must still
 * render, not blank the screen it appears on.
 */
export function formatMoney(money: Money): string {
	try {
		const format = new Intl.NumberFormat(locale(), {
			style: "currency",
			currency: money.currency,
		});
		const exponent = format.resolvedOptions().maximumFractionDigits ?? 2;
		return format.format(money.minor / 10 ** exponent);
	} catch {
		return `${(money.minor / 100).toFixed(2)} ${money.currency}`;
	}
}

export function formatTemp(celsius: number, display: TempDisplay = "C", digits = 1): string {
	const value = display === "F" ? celsius * 1.8 + 32 : celsius;
	return `${value.toFixed(digits)} °${display}`;
}

/** Gravity is stored and shown as SG: four decimals, as every brewing chart does. */
export function formatGravity(sg: number): string {
	return sg.toFixed(4);
}

export function formatVolume(litres: number, digits = 2): string {
	return litres < 1 ? `${(litres * 1000).toFixed(0)} mL` : `${litres.toFixed(digits)} L`;
}

export function formatMass(grams: number, digits = 1): string {
	return grams >= 1000 ? `${(grams / 1000).toFixed(digits)} kg` : `${grams.toFixed(digits)} g`;
}
