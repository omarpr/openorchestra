export function defaultDeviceName(ua: string = navigator.userAgent): string {
  if (/iPad/i.test(ua)) return "iPad";
  if (/iPhone|iPod/i.test(ua)) return "iPhone";
  if (/Android/i.test(ua)) return /Mobile/i.test(ua) ? "Android phone" : "Android tablet";
  // iPadOS 13+ reports as Macintosh; touch points distinguish it.
  if (/Macintosh/i.test(ua) && typeof navigator !== "undefined" && navigator.maxTouchPoints > 1) return "iPad";
  return "Browser";
}
