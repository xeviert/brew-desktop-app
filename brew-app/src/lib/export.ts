/**
 * Phase 5e. The save dialog, then a command that writes. The frontend only ever holds
 * the path the user picked; the file is Rust's.
 */
import { save } from "@tauri-apps/plugin-dialog";
import { toDate } from "$lib/query";

/**
 * `SELTZER-001 readings 2026-09-26.csv`. Characters Windows refuses in a file name —
 * a batch code or query name may have any — become `-`.
 */
export function csvName(parts: string[], now: number = Date.now() / 1000): string {
	const stem = parts
		.map((p) => p.trim())
		.filter(Boolean)
		.join(" ")
		.replace(/[<>:"/\|?*\x00-\x1f]/g, "-");
	return `${stem} ${toDate(now)}.csv`;
}

/** `false` when the dialog was cancelled: nothing written, nothing to report. */
export async function saveCsv(
	defaultName: string,
	write: (path: string) => Promise<void>,
): Promise<boolean> {
	const path = await save({
		defaultPath: defaultName,
		filters: [{ name: "CSV", extensions: ["csv"] }],
	});
	if (path === null) return false;
	await write(path);
	return true;
}
