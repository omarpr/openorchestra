import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, UnauthorizedError } from "@/lib/api";

const clearToken = vi.fn();
vi.mock("@/lib/token", () => ({
  getToken: () => "tok",
  clearToken: () => clearToken(),
}));

import { useDevice } from "./useDevice";

afterEach(() => {
  clearToken.mockReset();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("useDevice", () => {
  it("retries transient network and 503 failures until /api/me succeeds", async () => {
    const fetch = vi
      .fn()
      .mockRejectedValueOnce(new TypeError("network error"))
      .mockResolvedValueOnce(Response.json({ error: "unavailable" }, { status: 503 }))
      .mockResolvedValueOnce(Response.json({ name: "Phone", read_only: false, voice_input: true }));
    vi.stubGlobal("fetch", fetch);

    const { result } = renderHook(() => useDevice([5, 5, 5]));
    expect(result.current).toBeNull();
    await waitFor(() => expect(result.current).toEqual({ name: "Phone", read_only: false, voice_input: true }));
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it("stops after bounded retries on persistent 503 failures", async () => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ error: "unavailable" }, { status: 503 }));
    vi.stubGlobal("fetch", fetch);

    const { result } = renderHook(() => useDevice([5, 5]));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(3));
    await new Promise((r) => setTimeout(r, 20));
    expect(fetch).toHaveBeenCalledTimes(3);
    expect(result.current).toBeNull();
  });

  it("does not retry 401 unauthorized responses and still clears the token", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", fetch);

    const { result } = renderHook(() => useDevice([5, 5]));
    await waitFor(() => expect(clearToken).toHaveBeenCalledTimes(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(result.current).toBeNull();
  });

  it("exported ApiError and UnauthorizedError types remain distinct", () => {
    expect(new ApiError(503, "unavailable")).toBeInstanceOf(ApiError);
    expect(new UnauthorizedError()).toBeInstanceOf(UnauthorizedError);
  });
});
