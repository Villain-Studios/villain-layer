import { writeText } from "@tauri-apps/plugin-clipboard-manager";

/**
 * Put text on the system clipboard.
 *
 * The browser Clipboard API is unreliable in a Tauri webview (no secure
 * context, and macOS often refuses the call), so this goes through the
 * clipboard plugin that talks to the OS pasteboard directly.
 */
export async function copyText(text: string): Promise<void> {
  await writeText(text);
}
