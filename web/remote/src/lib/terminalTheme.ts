/** xterm colors for light or dark appearance. */
export function terminalTheme(dark: boolean) {
  return dark
    ? { background: "#0a0a0a", foreground: "#e5e5e5", cursor: "#e5e5e5" }
    : { background: "#ffffff", foreground: "#171717", cursor: "#171717", selectionBackground: "#bfdbfe" };
}
