/**
 * Host-platform detection for the few places the UI must diverge by OS.
 *
 * The window chrome is the main case: on macOS the window is frameless with the
 * native traffic lights overlaid (the title bar leaves a 78px safe area on the
 * left); on Windows the window is undecorated and the app draws its own
 * minimize / maximize / close controls on the right (see `tauri.windows.conf.json`
 * and `TitleBar.tsx`).
 *
 * Detection reads the WebView user agent rather than pulling in the `os` plugin —
 * a zero-dependency check that is reliable in the desktop runtimes we ship
 * (WebView2 on Windows, WKWebView on macOS) and degrades sensibly in the plain
 * browser dev preview, where it simply reports the developer's own OS.
 */

export type Platform = "macos" | "windows" | "linux";

function detect(): Platform {
  // `navigator` is always present in our WebView and in the dev browser.
  const ua = typeof navigator === "undefined" ? "" : navigator.userAgent;
  if (ua.includes("Windows")) return "windows";
  if (ua.includes("Mac OS X") || ua.includes("Macintosh")) return "macos";
  return "linux";
}

/** The platform the app is running on, resolved once at module load. */
export const platform: Platform = detect();

/** True on Windows — the app draws its own window controls there. */
export const isWindows = platform === "windows";

/** True on macOS — the title bar reserves the traffic-light safe area. */
export const isMacOS = platform === "macos";
