import { useEffect, useState } from "react";
import { type Device, me } from "@/lib/api";

/** The paired device's permissions and capabilities, once the host replies. */
export function useDevice(): Device | null {
  const [device, setDevice] = useState<Device | null>(null);
  useEffect(() => {
    let live = true;
    me()
      .then((device) => live && setDevice(device))
      .catch(() => {
        /* 401 already cleared the token; other errors keep controls hidden */
      });
    return () => {
      live = false;
    };
  }, []);
  return device;
}
