import { useState } from "react";
import { Send } from "lucide-react";
import { toast } from "sonner";
import { ApiError, sendText } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function QuickReply({ id, disabled, inputRef }: { id: string; disabled: boolean; inputRef?: React.Ref<HTMLInputElement> }) {
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
          ? "Agent stopped"
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
        placeholder={disabled ? "Agent stopped" : "Message the agent"}
        aria-label="Message the agent"
        enterKeyHint="send"
        autoCapitalize="off"
        autoCorrect="off"
        className="h-11 min-w-0 flex-1 text-base"
      />
      <Button type="submit" size="icon" aria-label="Send" disabled={disabled || busy || !text.trim()} className="size-11 shrink-0">
        <Send />
      </Button>
    </form>
  );
}
