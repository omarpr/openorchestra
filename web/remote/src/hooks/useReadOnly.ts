import { useEffect, useState } from "react";
import { me } from "@/lib/api";

/** Whether this device is watch-only, per `/api/me`; `null` until it answers. */
export function useReadOnly(): boolean | null {
  const [readOnly, setReadOnly] = useState<boolean | null>(null);
  useEffect(() => {
    let live = true;
    me()
      .then((m) => live && setReadOnly(m.read_only))
      .catch(() => {
        /* 401 already cleared the token; other errors keep controls hidden */
      });
    return () => {
      live = false;
    };
  }, []);
  return readOnly;
}
