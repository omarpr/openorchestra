import { type TouchEvent, useRef, useState } from "react";

const dist = (e: TouchEvent) =>
  Math.hypot(e.touches[0].clientX - e.touches[1].clientX, e.touches[0].clientY - e.touches[1].clientY);

/** Two-finger zoom between 1× and `max`×; spread the returned handlers on the zoomable element. */
export function usePinchZoom(max: number) {
  const [zoom, setZoom] = useState(1);
  const zoomRef = useRef(1);
  zoomRef.current = zoom;
  const pinch = useRef<{ d: number; z: number } | null>(null);
  const handlers = {
    onTouchStart(e: TouchEvent) {
      pinch.current = e.touches.length === 2 ? { d: dist(e), z: zoomRef.current } : null;
    },
    onTouchMove(e: TouchEvent) {
      if (e.touches.length === 2 && pinch.current && pinch.current.d > 0) {
        setZoom(Math.min(max, Math.max(1, (pinch.current.z * dist(e)) / pinch.current.d)));
      }
    },
    onTouchEnd() {
      pinch.current = null;
    },
  };
  return { zoom, zoomRef, handlers };
}
