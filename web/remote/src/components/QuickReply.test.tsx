import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { QuickReply } from "./QuickReply";

afterEach(() => vi.unstubAllGlobals());

function renderComposer() {
  const { container } = render(<QuickReply id="a1" disabled={false} name="Codex" track={{ queued: false }} />);
  const picker = container.querySelector<HTMLInputElement>('input[type="file"]');
  if (!picker) throw new Error("Screenshot picker is missing");
  const screenshot = new File(["example"], "screenshot.png", { type: "image/png" });
  const send = screen.getByRole("button", { name: "Send" });
  return { picker, screenshot, send };
}

it("sends an image-only draft only after Send", async () => {
  const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
  vi.stubGlobal("fetch", fetch);
  const { picker, screenshot, send } = renderComposer();
  const openPicker = vi.spyOn(picker, "click");
  await userEvent.click(screen.getByRole("button", { name: "Attach screenshot" }));
  expect(openPicker).toHaveBeenCalled();
  await userEvent.upload(picker, screenshot);
  expect(screen.getByText("screenshot.png")).toBeInTheDocument();
  expect(fetch).not.toHaveBeenCalled();
  await userEvent.click(send);
  await waitFor(() => expect(screen.queryByText("screenshot.png")).toBeNull());
  expect(fetch).toHaveBeenCalledWith(
    "/api/agents/a1/send-screenshot",
    expect.objectContaining({ method: "POST", body: JSON.stringify({ text: "", image: "ZXhhbXBsZQ==" }) }),
  );
});

it("keeps the screenshot and caption for retry when sending fails", async () => {
  const fetch = vi.fn().mockRejectedValue(new Error("offline"));
  vi.stubGlobal("fetch", fetch);
  const { picker, screenshot, send } = renderComposer();
  const caption = screen.getByRole("textbox", { name: "Message Codex" });
  await userEvent.type(caption, "What happened here?");
  await userEvent.upload(picker, screenshot);
  await userEvent.click(send);
  await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(send).toBeEnabled());
  expect(screen.getByText("screenshot.png")).toBeInTheDocument();
  expect(caption).toHaveValue("What happened here?");
  await userEvent.click(screen.getByRole("button", { name: "Remove screenshot" }));
  expect(screen.queryByText("screenshot.png")).toBeNull();
});

it("keeps a long draft scrollable and sends it with Enter", async () => {
  const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
  vi.stubGlobal("fetch", fetch);
  render(<QuickReply id="a1" disabled={false} name="Codex" />);
  const draft = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Message Codex" });
  expect(draft.tagName).toBe("TEXTAREA");
  expect(draft).toHaveClass("overflow-y-auto", "overscroll-y-contain");
  Object.defineProperty(draft, "scrollHeight", { configurable: true, value: 300 });
  draft.scrollTop = 32;
  const longText = "A long note for Codex. ".repeat(30).trim();
  fireEvent.change(draft, { target: { value: longText } });
  expect(draft.style.height).toBe("300px");
  expect(draft.scrollTop).toBe(32);

  const user = userEvent.setup();
  draft.focus();
  draft.setSelectionRange(longText.length, longText.length);
  await user.keyboard("{Shift>}{Enter}{/Shift}Another line");
  expect(draft).toHaveValue(`${longText}\nAnother line`);
  expect(fetch).not.toHaveBeenCalled();
  await user.keyboard("{Enter}");
  await waitFor(() => expect(draft).toHaveValue(""));
  expect(fetch).toHaveBeenCalledWith(
    "/api/agents/a1/send",
    expect.objectContaining({ body: JSON.stringify({ text: `${longText}\nAnother line` }) }),
  );
});
