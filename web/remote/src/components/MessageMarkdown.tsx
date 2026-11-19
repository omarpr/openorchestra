import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

// GFM's autolinks construct lookbehind regexes at render time (Safari 16.4+).
const remarkPlugins = (() => {
  try {
    // A literal would fail at parse time, before the catch can run.
    const lookbehind = "(?<!a)b";
    return new RegExp(lookbehind).test("b") ? [remarkGfm] : [];
  } catch {
    return [];
  }
})();

const PROSE =
  "max-w-full break-words text-sm leading-relaxed [&_a]:underline [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:font-mono [&_code]:text-[0.85em] [&_del]:opacity-70 [&_h1]:text-base [&_h1]:font-semibold [&_h2]:font-semibold [&_h3]:font-semibold [&_li]:ml-5 [&_ol]:list-decimal [&_p]:my-2 [&_pre]:my-2 [&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:bg-muted [&_pre]:p-2 [&_pre_code]:bg-transparent [&_pre_code]:p-0 [&_ul]:list-disc [&_.contains-task-list]:list-none [&_.task-list-item]:ml-0";

const components: Components = {
  a: ({ node: _n, ...p }) => <a {...p} target="_blank" rel="noreferrer noopener" />,
  // Tables scroll sideways on a narrow screen rather than squashing columns.
  table: ({ node: _n, ...p }) => (
    <div className="my-2 max-w-full overflow-x-auto rounded-md border">
      <table {...p} className="w-max min-w-full border-collapse text-left text-[0.85em]" />
    </div>
  ),
  thead: ({ node: _n, ...p }) => <thead {...p} className="bg-muted" />,
  th: ({ node: _n, ...p }) => <th {...p} className="border-b px-2 py-1.5 font-semibold" />,
  td: ({ node: _n, ...p }) => <td {...p} className="border-t px-2 py-1.5 align-top" />,
};

/** An agent's message: GitHub-flavored Markdown (tables, task lists, strikethrough), no images or raw HTML. */
export function MessageMarkdown({ text }: { text: string }) {
  return (
    <div className={PROSE}>
      <Markdown remarkPlugins={remarkPlugins} disallowedElements={["img"]} components={components}>
        {text}
      </Markdown>
    </div>
  );
}
