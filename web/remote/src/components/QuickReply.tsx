import { ImagePlus, Send, X } from "lucide-react";
import { type RefObject, useEffect, useLayoutEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { VoiceButton, VoiceFeedback } from "@/components/agent/VoiceButton";
import { Button } from "@/components/ui/button";
import { useVoiceDraft } from "@/hooks/useVoiceDraft";
import { ApiError, MAX_SCREENSHOT_BYTES, sendScreenshot, sendText } from "@/lib/api";
import { sendErrorText, sendTracked } from "@/lib/send";
import { canRecordVoice } from "@/lib/voice";

function screenshotError(err: unknown): string {
  if (err instanceof ApiError) {
    switch (err.code) {
      case "unsupported_image":
        return "Choose a PNG, JPEG, or WebP image";
      case "unreadable_image":
        return "Couldn't read that screenshot";
      case "screenshot_storage_full":
        return "Screenshot storage on your Mac is full";
      case "too_large":
        return "The screenshot or its caption is too large";
    }
  }
  return sendErrorText(err, false);
}

function sizeDraft(draft: HTMLTextAreaElement) {
  const scrollTop = draft.scrollTop;
  draft.style.height = "auto";
  const borders = draft.offsetHeight - draft.clientHeight;
  draft.style.height = `${draft.scrollHeight + borders}px`;
  draft.scrollTop = scrollTop;
}

/** `name` is what the tab runs ("Claude Code"); `null` for a shell. */
export function QuickReply({
  id,
  disabled,
  name = null,
  inputRef,
  track,
  voiceInput = false,
}: {
  id: string;
  disabled: boolean;
  name?: string | null;
  inputRef?: RefObject<HTMLTextAreaElement>;
  /**
   * Show each message in the conversation until the agent's transcript has it (`queued`: the
   * agent is working, so it waits its turn). Without it, failures are a toast.
   */
  track?: { queued: boolean };
  voiceInput?: boolean;
}) {
  const shell = name === null;
  const label = shell ? "Type a command" : `Message ${name || "the agent"}`;
  const stoppedText = shell ? "Shell closed" : "Agent stopped";
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [screenshot, setScreenshot] = useState<File>();
  const picker = useRef<HTMLInputElement>(null);
  const localDraft = useRef<HTMLTextAreaElement>(null);
  const draftRef = inputRef ?? localDraft;
  const voice = useVoiceDraft((dictation) => {
    setText((draft) => (draft.trimEnd() ? `${draft.trimEnd()} ${dictation}` : dictation));
  });
  const dictating = voice.phase !== "idle";
  const showVoice = voiceInput && canRecordVoice();

  useLayoutEffect(() => {
    if (draftRef.current) sizeDraft(draftRef.current);
  });
  useEffect(() => {
    const resize = () => {
      if (draftRef.current) sizeDraft(draftRef.current);
    };
    window.addEventListener("resize", resize);
    return () => window.removeEventListener("resize", resize);
  }, [draftRef]);

  const chooseScreenshot = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    if (file.size > MAX_SCREENSHOT_BYTES) {
      toast.error("Choose a screenshot smaller than 8 MB");
      return;
    }
    setScreenshot(file);
  };

  const submitScreenshot = async (caption: string, file: File) => {
    setBusy(true);
    try {
      await sendScreenshot(id, caption, file);
      setScreenshot(undefined);
      setText("");
      toast.success("Screenshot sent");
    } catch (err) {
      toast.error(screenshotError(err));
    } finally {
      setBusy(false);
    }
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const t = text.trim();
    if (busy || disabled || dictating) return;
    if (screenshot) {
      await submitScreenshot(t, screenshot);
      return;
    }
    if (!t) return;
    if (track) {
      // The bubble takes over: sending, sent, queued, or failed with Retry.
      setText("");
      await sendTracked(id, t, { queued: track.queued, shell });
      return;
    }
    setBusy(true);
    try {
      await sendText(id, t);
      setText("");
    } catch (err) {
      toast.error(sendErrorText(err, shell));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className="flex min-w-0 flex-1 flex-col gap-1">
      {showVoice && <VoiceFeedback voice={voice} disabled={disabled || busy} />}
      {screenshot && (
        <div className="flex max-w-full items-center gap-1 self-start rounded-md border pl-2 text-sm">
          <span className="min-w-0 truncate">{screenshot.name}</span>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            aria-label="Remove screenshot"
            disabled={busy}
            className="size-9 shrink-0"
            onClick={() => setScreenshot(undefined)}
          >
            <X />
          </Button>
        </div>
      )}
      <div className="flex min-w-0 items-end gap-2">
        {!shell && (
          <>
            <input
              ref={picker}
              type="file"
              accept="image/png,image/jpeg,image/webp"
              onChange={chooseScreenshot}
              hidden
            />
            <Button
              type="button"
              variant="outline"
              size="icon"
              aria-label="Attach screenshot"
              disabled={disabled || busy || dictating}
              className="size-11 shrink-0"
              onClick={() => picker.current?.click()}
            >
              <ImagePlus />
            </Button>
          </>
        )}
        {showVoice && <VoiceButton voice={voice} disabled={disabled || busy} />}
        <textarea
          ref={draftRef}
          rows={1}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== "Enter" || e.shiftKey || e.nativeEvent.isComposing) return;
            void submit(e);
          }}
          disabled={disabled || (busy && Boolean(screenshot))}
          placeholder={disabled ? stoppedText : label}
          aria-label={label}
          enterKeyHint="send"
          autoCapitalize="off"
          autoCorrect="off"
          className="max-h-[min(8rem,calc(var(--oo-vv-height,100vh)*0.3))] min-h-11 min-w-0 flex-1 touch-pan-y resize-none overflow-y-auto overscroll-y-contain rounded-md border border-input bg-transparent px-3 py-2.5 text-base leading-5 shadow-xs placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
        />
        <Button
          type="submit"
          size="icon"
          aria-label="Send"
          aria-busy={busy}
          disabled={disabled || busy || dictating || (!screenshot && !text.trim())}
          className="size-11 shrink-0"
        >
          <Send />
        </Button>
      </div>
    </form>
  );
}
