import { useEffect, useRef, useState } from "react";
import { ApiError, type Device, me, UnauthorizedError } from "@/lib/api";

const RETRY_DELAYS_MS = [500, 1000, 2000, 4000];

function isTransient(err: unknown): boolean {
  if (err instanceof UnauthorizedError) return false;
  if (err instanceof ApiError) return err.status >= 500;
  return true;
}

/** The paired device's permissions and capabilities, once the host replies. */
export function useDevice(retryDelays = RETRY_DELAYS_MS): Device | null {
  const [device, setDevice] = useState<Device | null>(null);
  const delays = useRef(retryDelays).current;
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;

    function load() {
      me()
        .then((result) => {
          if (live) setDevice(result);
        })
        .catch((err: unknown) => {
          // 401 already cleared the token; other non-transient errors keep controls hidden.
          if (!live || !isTransient(err) || attempt >= delays.length) return;
          const delay = delays[attempt++];
          timer = setTimeout(() => {
            if (live) load();
          }, delay);
        });
    }

    load();
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [delays]);
  return device;
}
