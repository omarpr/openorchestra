import { Send } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ApiError, sendText } from "@/lib/api";

/** `name` is what the tab runs ("Claude Code"); `null` for a shell. */
export function QuickReply({
  id,
  disabled,
  name = null,
  inputRef,
}: {
  id: string;
  disabled: boolean;
  name?: string | null;
  inputRef?: React.Ref<HTMLInputElement>;
}) {
  const shell = name === null;
  const label = shell ? "Type a command" : `Message ${name || "the agent"}`;
  const stoppedText = shell ? "Shell closed" : "Agent stopped";
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const t = text.trim();
    if (!t || busy || disabled) return;
    setBusy(true);
    try {
      await sendText(id, t);
      setText("");
    } catch (err) {
      toast.error(
        err instanceof ApiError && err.code === "stopped"
          ? stoppedText
          : err instanceof ApiError && err.code === "read_only"
            ? "This device is watch only"
            : "Could not send. Try again.",
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className="flex min-w-0 flex-1 gap-2">
      <Input
        ref={inputRef}
        value={text}
        onChange={(e) => setText(e.target.value)}
        disabled={disabled}
        placeholder={disabled ? stoppedText : label}
        aria-label={label}
        enterKeyHint="send"
        autoCapitalize="off"
        autoCorrect="off"
        className="h-11 min-w-0 flex-1 text-base"
      />
      <Button
        type="submit"
        size="icon"
        aria-label="Send"
        disabled={disabled || busy || !text.trim()}
        className="size-11 shrink-0"
      >
        <Send />
      </Button>
    </form>
  );
}
