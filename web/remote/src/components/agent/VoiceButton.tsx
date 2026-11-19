import { Mic, Square, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { useVoiceDraft, VoicePhase } from "@/hooks/useVoiceDraft";

const voiceStatus: Record<VoicePhase, string> = {
  idle: "",
  permission: "Waiting for microphone…",
  recording: "Recording…",
  transcribing: "Transcribing on your Mac…",
};

export function VoiceFeedback({ voice, disabled }: { voice: ReturnType<typeof useVoiceDraft>; disabled: boolean }) {
  if (voice.failure) {
    return (
      <div className="flex flex-wrap items-center gap-x-2 px-1">
        <p role="status" className="text-sm text-muted-foreground">
          {voice.failure}
        </p>
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-label="Retry transcription"
          onClick={voice.retry}
          disabled={disabled}
        >
          Retry
        </Button>
        <Button type="button" variant="ghost" size="sm" aria-label="Discard recording" onClick={voice.discard}>
          Discard
        </Button>
      </div>
    );
  }
  if (voice.phase === "idle") return null;
  return (
    <p role="status" className="px-1 text-sm text-muted-foreground">
      {voiceStatus[voice.phase]}
    </p>
  );
}

export function VoiceButton({ voice, disabled }: { voice: ReturnType<typeof useVoiceDraft>; disabled: boolean }) {
  const { phase, start, stop, cancel } = voice;
  const action = {
    idle: { text: "Dictate message", Icon: Mic, run: start },
    recording: { text: "Stop recording", Icon: Square, run: stop },
    permission: { text: "Cancel voice input", Icon: X, run: cancel },
    transcribing: { text: "Cancel voice input", Icon: X, run: cancel },
  }[phase];
  return (
    <Button
      type="button"
      variant="outline"
      size="icon"
      aria-label={action.text}
      disabled={phase === "idle" && (disabled || Boolean(voice.failure))}
      className={phase === "recording" ? "size-11 shrink-0 text-destructive" : "size-11 shrink-0"}
      onClick={action.run}
    >
      <action.Icon />
    </Button>
  );
}
