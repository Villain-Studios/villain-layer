import {
  isPermissionGranted,
  requestPermission,
} from "@tauri-apps/plugin-notification";

// The banners themselves are the backend's — `news.rs` for reviews and
// tickets, `attention.rs` for agents — since a hidden webview's timers are
// the ones macOS throttles. What is left here is asking for the permission
// while someone is looking, so the first banner is not also the question.

let asked = false;
let granted = false;

/** Ask once per launch. A refusal stays a refusal until the app is opened again. */
async function allowed(): Promise<boolean> {
  if (granted) return true;
  if (await isPermissionGranted()) {
    granted = true;
    return true;
  }
  if (asked) return false;
  asked = true;
  granted = (await requestPermission()) === "granted";
  return granted;
}

/** Ask for permission while the window is open, so a later banner is not the first time. */
export function prepareNotifications(): Promise<boolean> {
  return allowed();
}
