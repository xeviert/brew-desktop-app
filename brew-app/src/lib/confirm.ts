/**
 * A yes/no question before a destructive action.
 *
 * Not `window.confirm`: in Tauri that returns a Promise, which is always truthy, so
 * `if (!confirm(...)) return` never returns and the action runs unasked. The dialog
 * plugin's `ask` is the real prompt; it needs `dialog:allow-ask` in the capability.
 */
import { ask } from "@tauri-apps/plugin-dialog";

export function confirmAction(message: string, okLabel: string): Promise<boolean> {
	return ask(message, { kind: "warning", okLabel, cancelLabel: "Cancel" });
}
