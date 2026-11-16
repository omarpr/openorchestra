export function isStandalone(): boolean {
  const mq = typeof window.matchMedia === "function" && window.matchMedia("(display-mode: standalone)").matches;
  return mq || (navigator as Navigator & { standalone?: boolean }).standalone === true;
}

/** iPhone/iPad/iPod, including iPadOS 13+ which reports as Macintosh (told apart by touch points). */
export function isIOS(ua: string = navigator.userAgent, maxTouchPoints: number = navigator.maxTouchPoints): boolean {
  if (/iPhone|iPad|iPod/i.test(ua)) return true;
  return /Macintosh/i.test(ua) && maxTouchPoints > 1;
}
