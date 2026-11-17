/** The Mac's name, stamped into the shell by the server (`oo-host` meta). */
export function hostName(): string | null {
  const c = document.querySelector<HTMLMetaElement>('meta[name="oo-host"]')?.content.trim();
  return c ? c : null;
}
