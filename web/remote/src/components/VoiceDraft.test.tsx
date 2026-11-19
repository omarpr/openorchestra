import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { QuickReply } from "./QuickReply";

const recordVoice = vi.hoisted(() => vi.fn<typeof import("@/lib/voice").recordVoice>());
vi.mock("@/lib/voice", () => ({ canRecordVoice: () => true, recordVoice }));

afterEach(() => {
  vi.unstubAllGlobals();
  recordVoice.mockReset();
});

it("appends dictated text to the draft and leaves it for review before sending", async () => {
  const clip = new Blob(["recording"], { type: "audio/wav" });
  recordVoice.mockImplementation((_signal, onRecording) => new Promise((resolve) => onRecording(() => resolve(clip))));
  const fetch = vi
    .fn()
    .mockResolvedValueOnce(Response.json({ text: "from my phone" }))
    .mockResolvedValueOnce(new Response(null, { status: 204 }));
  vi.stubGlobal("fetch", fetch);
  const user = userEvent.setup();
  render(<QuickReply id="a1" disabled={false} name="Codex" voiceInput />);
  const draft = screen.getByRole("textbox", { name: "Message Codex" });
  const send = screen.getByRole("button", { name: "Send" });
  await user.type(draft, "A note");
  await user.click(screen.getByRole("button", { name: "Dictate message" }));
  expect(screen.getByRole("status")).toHaveTextContent("Recording");
  expect(send).toBeDisabled();
  expect(fetch).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Stop recording" }));
  await waitFor(() => expect(draft).toHaveValue("A note from my phone"));
  expect(fetch).toHaveBeenCalledExactlyOnceWith("/api/voice", expect.objectContaining({ method: "POST", body: clip }));
  await user.type(draft, ".");
  await user.click(send);
  expect(fetch).toHaveBeenLastCalledWith(
    "/api/agents/a1/send",
    expect.objectContaining({ body: JSON.stringify({ text: "A note from my phone." }) }),
  );
});

it("cancels microphone capture when leaving the composer", async () => {
  const fetch = vi.fn();
  vi.stubGlobal("fetch", fetch);
  recordVoice.mockImplementation(
    (signal, onRecording) =>
      new Promise((_resolve, reject) => {
        signal.addEventListener("abort", () => reject(signal.reason), { once: true });
        onRecording(() => undefined);
      }),
  );
  const view = render(<QuickReply id="a1" disabled={false} name="Codex" voiceInput />);
  await userEvent.click(screen.getByRole("button", { name: "Dictate message" }));
  const signal = recordVoice.mock.calls[0][0];
  expect(signal.aborted).toBe(false);
  view.unmount();
  expect(signal.aborted).toBe(true);
  expect(fetch).not.toHaveBeenCalled();
});

it("retries the same clip after the Mac is busy or times out", async () => {
  const clip = new Blob(["recording"], { type: "audio/wav" });
  recordVoice.mockImplementation((_signal, onRecording) => new Promise((resolve) => onRecording(() => resolve(clip))));
  const fetch = vi
    .fn()
    .mockResolvedValueOnce(Response.json({ error: "voice_busy" }, { status: 429 }))
    .mockResolvedValueOnce(Response.json({ error: "voice_timeout" }, { status: 504 }))
    .mockResolvedValueOnce(Response.json({ text: "It worked" }));
  vi.stubGlobal("fetch", fetch);
  const user = userEvent.setup();
  render(<QuickReply id="a1" disabled={false} name="Codex" voiceInput />);
  await user.click(screen.getByRole("button", { name: "Dictate message" }));
  await user.click(screen.getByRole("button", { name: "Stop recording" }));
  await user.click(await screen.findByRole("button", { name: "Retry transcription" }));
  await screen.findByText("Transcription timed out");
  await user.click(screen.getByRole("button", { name: "Retry transcription" }));
  await waitFor(() => expect(screen.getByRole("textbox", { name: "Message Codex" })).toHaveValue("It worked"));
  expect(recordVoice).toHaveBeenCalledTimes(1);
  expect(fetch.mock.calls.map(([path, init]) => [path, init.body])).toEqual(
    Array.from({ length: 3 }, () => ["/api/voice", clip]),
  );
  expect(screen.queryByRole("button", { name: "Retry transcription" })).not.toBeInTheDocument();
});

it("lets the user discard a failed clip without sending another request", async () => {
  const clip = new Blob(["recording"], { type: "audio/wav" });
  recordVoice.mockImplementation((_signal, onRecording) => new Promise((resolve) => onRecording(() => resolve(clip))));
  const fetch = vi.fn().mockRejectedValue(new TypeError("offline"));
  vi.stubGlobal("fetch", fetch);
  const user = userEvent.setup();
  render(<QuickReply id="a1" disabled={false} name="Codex" voiceInput />);
  await user.click(screen.getByRole("button", { name: "Dictate message" }));
  await user.click(screen.getByRole("button", { name: "Stop recording" }));
  await user.click(await screen.findByRole("button", { name: "Discard recording" }));
  expect(screen.queryByRole("button", { name: "Retry transcription" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Dictate message" })).toBeEnabled();
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("keeps Stop enabled when disabled becomes true during recording", async () => {
  const clip = new Blob(["recording"], { type: "audio/wav" });
  recordVoice.mockImplementation((_signal, onRecording) => new Promise((resolve) => onRecording(() => resolve(clip))));
  const fetch = vi.fn().mockResolvedValueOnce(Response.json({ text: "hello" }));
  vi.stubGlobal("fetch", fetch);
  const user = userEvent.setup();
  const view = render(<QuickReply id="a1" disabled={false} name="Codex" voiceInput />);
  await user.click(screen.getByRole("button", { name: "Dictate message" }));
  view.rerender(<QuickReply id="a1" disabled name="Codex" voiceInput />);
  const stop = screen.getByRole("button", { name: "Stop recording" });
  expect(stop).toBeEnabled();
  await user.click(stop);
  await waitFor(() => expect(screen.getByRole("textbox", { name: "Message Codex" })).toHaveValue("hello"));
});
