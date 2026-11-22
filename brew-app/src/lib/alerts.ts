/**
 * Phase 6. Wording for alerts. `brewd` writes each alert's `detail` (the numbers it
 * saw); everything else a screen says about an alert is decided here.
 */
import type { AlertRow, AlertRule } from "./api";
import { formatTimestamp } from "./units";

const RULES: Record<AlertRule, string> = {
	finished: "Fermentation finished",
	stalled: "Fermentation stalled",
	temp_high: "Too warm",
	temp_low: "Too cold",
	silent: "Device silent",
	sugar_break: "⅓ sugar break",
	target_sg: "Target gravity reached",
	temp_rate: "Temperature moving fast",
};

/** Rules worth acting on now, as opposed to today. Matches `brewd`'s push priority. */
const URGENT: ReadonlySet<AlertRule> = new Set(["stalled", "temp_high", "temp_low", "temp_rate", "silent"]);

export const ruleText = (rule: AlertRule): string => RULES[rule] ?? rule.replaceAll("_", " ");

export const isUrgent = (a: AlertRow): boolean => URGENT.has(a.rule);

/** "Too warm", or "ispindel-1 silent" for a device. */
export function alertTitle(a: AlertRow): string {
	return a.rule === "silent" && a.device ? `${a.device} silent` : ruleText(a.rule);
}

/**
 * Where an alert stands, as a phrase: cleared, acknowledged, snoozed, pushed, or none
 * of those yet. `now` in epoch seconds.
 */
export function alertState(a: AlertRow, now: number): string {
	if (a.cleared_at !== null) {
		const why = a.cleared_note ? ` — ${a.cleared_note}` : "";
		return `cleared ${formatTimestamp(a.cleared_at)}${why}`;
	}
	if (a.acked_at !== null) return "acknowledged";
	if (a.snoozed_until !== null && a.snoozed_until > now) {
		return `snoozed until ${formatTimestamp(a.snoozed_until)}`;
	}
	if (a.notified_at !== null) return "sent to phone";
	return "not sent yet";
}

/** Open alerts by batch id, for marking cards. */
export function openByBatch(alerts: AlertRow[]): Map<number, AlertRow[]> {
	const out = new Map<number, AlertRow[]>();
	for (const a of alerts) {
		if (a.cleared_at !== null) continue;
		const list = out.get(a.batch_id) ?? [];
		list.push(a);
		out.set(a.batch_id, list);
	}
	return out;
}

/** What the Settings pane says about the last alert round. */
export function syncText(status: {
	at: number | null;
	report: { watches: string; watched: number } | null;
	error_code: string | null;
	error: string | null;
}): string {
	if (status.at === null) return "not checked yet";
	if (status.error_code === "brewd.unconfigured") return "brewd URL not set";
	if (status.error) return status.error;
	const r = status.report;
	if (!r) return "";
	const watched = r.watched === 1 ? "1 batch" : `${r.watched} batches`;
	if (r.watches === "no_token") return `no token: brewd isn't told what to watch (${watched} would be)`;
	return `brewd watching ${watched}`;
}
