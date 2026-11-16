import { beforeEach, describe, expect, it, vi } from "vitest";
import { me, UnauthorizedError } from "./api";
import { getToken, setToken } from "./token";

describe("api", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("{}", { status: 401 })));
  });

  it("an authed call receiving 401 clears the token", async () => {
    setToken("tok");
    await expect(me()).rejects.toBeInstanceOf(UnauthorizedError);
    expect(getToken()).toBeNull();
  });
});
