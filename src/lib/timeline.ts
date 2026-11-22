/**
 * Phase 3 UI pass, U4. The quick-entry and event forms and the timeline's wording, pure;
 * `QuickEntry.svelte`, `EventForm.svelte` and `Timeline.svelte` own the calls.
 *
 * A reading is sent in the scale it was read in (`entry_unit`) and converted by
 * `db::measurement`, which also decides which correction runs and refuses a mix. Here
 * only "not a number" is caught. Temperatures are typed in `ui.temp_display` and sent
 * as °C.
 */
import type {
	Config,
	EntryUnit,
	EventInput,
	EventKind,
	Measurement,
	NewMeasurement,
	QuickEntryInput,
	TimelineItem,
} from "$lib/api";
import { fromLocalDateTime, localDateTime } from "$lib/batch";
import { formatGravityIn } from "$lib/dashboard";
import { parseNumber, type Result } from "$lib/reference";
import { formatGravity, formatTemp, fromDisplayTemp, toDisplayTemp, type TempDisplay } from "$lib/units";

/** Written by their own forms, not the event editor. */
type OwnForm = "pitch" | "step_done" | "reminder_done";

/** Everything but `pitch` and the ticks, which have their own forms. */
export const EVENT_KINDS: { value: Exclude<EventKind, OwnForm>; label: string }[] = [
	{ value: "note", label: "Note" },
	{ value: "nutrient_addition", label: "Nutrient addition" },
	{ value: "flavoring_addition", label: "Flavoring addition" },
	{ value: "temp_ramp", label: "Temperature ramp" },
	{ value: "cold_crash", label: "Cold crash" },
	{ value: "transfer", label: "Transfer" },
	{ value: "fining", label: "Fining" },
	{ value: "carbonation", label: "Carbonation" },
	{ value: "package", label: "Package" },
];

export function eventLabel(kind: EventKind): string {
	if (kind === "pitch") return "Pitch";
	if (kind === "step_done") return "Step done";
	if (kind === "reminder_done") return "Reminder done";
	return EVENT_KINDS.find((k) => k.value === kind)?.label ?? kind;
}

const show = (v: number, digits = 2) => String(Number(v.toFixed(digits)));
const text = (s: string): string | null => (s.trim() === "" ? null : s.trim());

// ------------------------------------------------------------- quick entry

export type GravityScale = "sg" | "plato" | "brix";
export type GravityInstrument = "hydrometer" | "refractometer";

export interface QuickForm {
	/** `datetime-local`. */
	at: string;
	gravity: string;
	/** A hydrometer's scale. A refractometer reads Brix and nothing else. */
	scale: GravityScale;
	instrument: GravityInstrument;
	/** Hydrometer only, in the display temperature. Blank is no correction. */
	sampleTemp: string;
	calibrationTemp: string;
	/** Refractometer only. */
	wortCorrection: string;
	ph: string;
	/** In the display temperature. */
	temp: string;
	/** g/L as tartaric acid. */
	ta: string;
	note: string;
}

const SCALE: Record<Config["ui"]["gravity_display"], GravityScale> = { sg: "sg", plato: "plato", brix: "brix" };

/** Blank but for the time (now) and the config's defaults for the two corrections. */
export function quickForm(config: Config, now: number): QuickForm {
	return {
		at: localDateTime(now),
		gravity: "",
		scale: SCALE[config.ui.gravity_display],
		instrument: "hydrometer",
		sampleTemp: "",
		calibrationTemp: show(toDisplayTemp(config.measurement.hydrometer_calibration_c, config.ui.temp_display), 1),
		wortCorrection: show(config.measurement.refractometer_wort_correction, 3),
		ph: "",
		temp: "",
		ta: "",
		note: "",
	};
}

export function toQuickEntry(form: QuickForm, temps: TempDisplay): Result<QuickEntryInput> {
	const bad: string[] = [];
	const num = (label: string, s: string): number | null => {
		const v = parseNumber(s);
		if (v !== null && Number.isNaN(v)) {
			bad.push(label);
			return null;
		}
		return v;
	};
	const ts = fromLocalDateTime(form.at);
	if (ts === null) bad.push("time");

	const measurements: NewMeasurement[] = [];
	const reading = (m: Partial<NewMeasurement> & Pick<NewMeasurement, "metric" | "entry_value" | "entry_unit">) =>
		measurements.push({ instrument: null, sample_temp_c: null, calibration_temp_c: null, wort_correction: null, ...m });

	const gravity = num("gravity", form.gravity);
	if (gravity !== null) {
		if (form.instrument === "refractometer") {
			reading({
				metric: "sg",
				entry_value: gravity,
				entry_unit: "brix",
				instrument: "refractometer",
				wort_correction: num("wort correction", form.wortCorrection),
			});
		} else {
			// Both temperatures or neither: a sample temperature is what asks for the correction.
			const sample = num("sample temperature", form.sampleTemp);
			const calibration = sample === null ? null : num("calibration temperature", form.calibrationTemp);
			if (sample !== null && calibration === null && parseNumber(form.calibrationTemp) === null) {
				return { ok: false, error: "A sample temperature needs the hydrometer's calibration temperature" };
			}
			reading({
				metric: "sg",
				entry_value: gravity,
				entry_unit: form.scale,
				instrument: "hydrometer",
				sample_temp_c: sample === null ? null : fromDisplayTemp(sample, temps),
				calibration_temp_c: calibration === null ? null : fromDisplayTemp(calibration, temps),
			});
		}
	}
	const ph = num("pH", form.ph);
	if (ph !== null) reading({ metric: "ph", entry_value: ph, entry_unit: "ph", instrument: "ph_meter" });
	const temp = num("temperature", form.temp);
	if (temp !== null) {
		const unit: EntryUnit = temps === "F" ? "f" : "c";
		reading({ metric: "temp_c", entry_value: temp, entry_unit: unit, instrument: "thermometer" });
	}
	const ta = num("TA", form.ta);
	if (ta !== null) reading({ metric: "ta", entry_value: ta, entry_unit: "g_per_l", instrument: "titration" });

	if (bad.length) return { ok: false, error: `Not readable: ${bad.join(", ")}` };
	const note = text(form.note);
	if (measurements.length === 0 && note === null) return { ok: false, error: "Nothing to record" };
	return { ok: true, value: { ts, measurements, note } };
}

// ------------------------------------------------------------------ events

export interface EventForm {
	at: string;
	kind: Exclude<EventKind, OwnForm>;
	note: string;
}

export function eventForm(event: { ts: number; kind: EventKind; note: string | null } | null, now: number): EventForm {
	return {
		at: localDateTime(event?.ts ?? now),
		kind: event && event.kind !== "pitch" && event.kind !== "step_done" && event.kind !== "reminder_done"
			? event.kind
			: "note",
		note: event?.note ?? "",
	};
}

export function toEventInput(form: EventForm): Result<EventInput> {
	const ts = fromLocalDateTime(form.at);
	if (ts === null) return { ok: false, error: "Not readable: time" };
	return { ok: true, value: { ts, kind: form.kind, note: text(form.note) } };
}

// -------------------------------------------------------------- the list

export interface Moment {
	ts: number;
	measurements: (TimelineItem & { type: "measurement" })[];
	events: (TimelineItem & { type: "event" })[];
}

/** One group per timestamp, newest first: a quick entry is one moment, not four rows. */
export function moments(items: TimelineItem[]): Moment[] {
	const out: Moment[] = [];
	for (const item of items) {
		let m = out.at(-1);
		if (!m || m.ts !== item.ts) {
			m = { ts: item.ts, measurements: [], events: [] };
			out.push(m);
		}
		if (item.type === "measurement") m.measurements.push(item);
		else m.events.push(item);
	}
	return out.reverse();
}

/** "t+26 h", "t+3 d 4 h", "t−2 h": where a moment sits against the pitch. */
export function sincePitch(ts: number, pitchedAt: number | null): string {
	if (pitchedAt === null) return "";
	const s = ts - pitchedAt;
	const hours = Math.floor(Math.abs(s) / 3_600);
	const body = hours < 48 ? `${hours} h` : `${Math.floor(hours / 24)} d ${hours % 24} h`;
	return s < 0 ? `t−${body}` : `t+${body}`;
}

const ENTRY_UNIT: Record<EntryUnit, string> = {
	sg: "SG",
	plato: "°P",
	brix: "°Bx",
	c: "°C",
	f: "°F",
	ph: "pH",
	g_per_l: "g/L",
};

/** The value as the batch reads it: gravity in the display unit, temperature in the
 * display scale. */
export function formatValue(m: Measurement & { display: number | null }, gravityUnit: string, temps: TempDisplay): string {
	switch (m.metric) {
		case "sg":
			return m.display === null ? "" : formatGravityIn(m.display, gravityUnit);
		case "temp_c":
			return formatTemp(m.value, temps);
		case "ph":
			return `pH ${m.value.toFixed(2)}`;
		case "ta":
			return `TA ${m.value.toFixed(1)} g/L`;
	}
}

/** What was typed and which correction ran, when that differs from the value shown. */
export function entryDetail(m: Measurement, temps: TempDisplay): string {
	if (m.metric !== "sg") return "";
	const typed = m.entry_unit === "sg" ? formatGravity(m.entry_value) : `${show(m.entry_value, 1)} ${ENTRY_UNIT[m.entry_unit]}`;
	if (m.instrument === "refractometer") {
		return `refractometer ${typed}, wort factor ${show(m.wort_correction ?? 1, 3)}`;
	}
	const corrected = m.sample_temp_c !== null && m.calibration_temp_c !== null;
	const parts = [m.instrument ?? ""];
	// The typed value only when it is not what the list already shows.
	if (corrected || m.entry_unit !== "sg") parts.push(typed);
	if (corrected) {
		parts.push(
			`at ${formatTemp(m.sample_temp_c!, temps)}, corrected to ${show(toDisplayTemp(m.calibration_temp_c!, temps), 1)} °${temps}`,
		);
	}
	return parts.filter(Boolean).join(" ");
}
