import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ApiError, transcribeVoice, UnauthorizedError } from "@/lib/api";
import { recordVoice } from "@/lib/voice";

export type VoicePhase = "idle" | "permission" | "recording" | "transcribing";
type VoiceOperation = { controller: AbortController; stop?: () => void; clip?: Blob };

function canRetry(error: unknown) {
  if (error instanceof UnauthorizedError) return false;
  if (!(error instanceof ApiError)) return true;
  return error.status === 408 || error.status === 429 || error.status >= 500;
}

function voiceError(error: unknown) {
  if (error instanceof DOMException && error.name === "NotAllowedError") return "Allow microphone access to dictate";
  if (error instanceof ApiError) {
    switch (error.code) {
      case "invalid_audio":
        return "Couldn't read that recording";
      case "voice_unavailable":
        return "Voice input is unavailable on your Mac";
      case "voice_model_unavailable":
        return "Your Mac couldn't prepare a speech model for its current language";
      case "voice_busy":
        return "Your Mac is already transcribing a recording";
      case "voice_timeout":
        return "Transcription timed out";
      case "read_only":
        return "This device is watch only";
    }
  }
  return "Couldn't transcribe that recording";
}

export function useVoiceDraft(onText: (text: string) => void) {
  const [phase, setPhase] = useState<VoicePhase>("idle");
  const [failed, setFailed] = useState<{ clip: Blob; message: string }>();
  const active = useRef<VoiceOperation>();

  const cancel = useCallback(() => {
    active.current?.controller.abort();
    active.current = undefined;
    setPhase("idle");
  }, []);

  useEffect(() => {
    function onVisibilityChange() {
      // Stop a live microphone on app switch. Once captured, the upload can finish.
      if (document.hidden && active.current && !active.current.clip) cancel();
    }
    document.addEventListener("visibilitychange", onVisibilityChange);
    window.addEventListener("pagehide", cancel);
    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.removeEventListener("pagehide", cancel);
      active.current?.controller.abort();
    };
  }, [cancel]);

  const transcribe = async (clip?: Blob) => {
    if (active.current) return;
    const operation: VoiceOperation = { controller: new AbortController(), clip };
    active.current = operation;
    setFailed(undefined);
    const { signal } = operation.controller;
    try {
      if (!operation.clip) {
        setPhase("permission");
        operation.clip = await recordVoice(signal, (stop) => {
          operation.stop = stop;
          setPhase("recording");
        });
      }
      signal.throwIfAborted();
      setPhase("transcribing");
      const text = (await transcribeVoice(operation.clip, signal)).trim();
      if (signal.aborted) return;
      if (text) onText(text);
      else toast.info("No speech detected");
    } catch (error) {
      if (signal.aborted) return;
      const message = voiceError(error);
      if (operation.clip && canRetry(error)) {
        setFailed({ clip: operation.clip, message });
      } else {
        toast.error(message);
      }
    } finally {
      if (active.current === operation) {
        active.current = undefined;
        setPhase("idle");
      }
    }
  };

  return {
    phase,
    failure: failed?.message,
    start: () => transcribe(),
    stop: () => active.current?.stop?.(),
    cancel,
    retry: () => {
      if (failed) void transcribe(failed.clip);
    },
    discard: () => setFailed(undefined),
  };
}
