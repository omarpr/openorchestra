/**
 * Placeholder for the agent terminal view (Task 7 replaces this file's body).
 * Contract: rendered with the selected agent id; it fills its container.
 */
export function AgentView({ id }: { id: string }) {
  return (
    <div className="flex h-full items-center justify-center p-6 text-sm text-muted-foreground" data-agent-id={id}>
      Terminal view coming soon
    </div>
  );
}
