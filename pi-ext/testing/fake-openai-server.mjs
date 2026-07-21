// A minimal OpenAI-compatible `/v1/chat/completions` SSE server standing in for
// LM Studio (architecture §5), so a real `pi` run can be driven end to end with
// no API key, no network, and deterministic output.
//
// It answers exactly two turns:
//   1. a `bash` tool call  -> exercises tool_execution_* and keeps the agent busy
//   2. a short text reply  -> ends the turn, so agent_end + settle -> idle
//
// A deliberate response delay (RESPONSE_DELAY_MS) keeps the agent in `running`
// long enough for a status sampler to observe the transition; without it the
// whole run finishes inside one sampling interval.
//
// Used by capture-lifecycle.sh. See docs/m2-pi-pin.md.

import http from "node:http";

const PORT = Number(process.env.FAKE_LLM_PORT ?? 1234);
const RESPONSE_DELAY_MS = Number(process.env.RESPONSE_DELAY_MS ?? 1500);
const MODEL = "omar-agent-v1";

let turn = 0;
const sse = (res, obj) => res.write(`data: ${JSON.stringify(obj)}\n\n`);

const server = http.createServer((req, res) => {
	if (req.url === "/v1/models") {
		res.writeHead(200, { "content-type": "application/json" });
		return res.end(JSON.stringify({ object: "list", data: [{ id: MODEL, object: "model" }] }));
	}

	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		res.writeHead(200, {
			"content-type": "text/event-stream",
			"cache-control": "no-cache",
			connection: "keep-alive",
		});
		const base = { id: "cmpl-1", object: "chat.completion.chunk", created: 0, model: MODEL };
		turn++;
		setTimeout(() => {
			if (turn === 1) {
				// Tool call, split across deltas the way real providers stream it.
				sse(res, {
					...base,
					choices: [
						{
							index: 0,
							delta: {
								role: "assistant",
								tool_calls: [
									{ index: 0, id: "call_1", type: "function", function: { name: "bash", arguments: "" } },
								],
							},
							finish_reason: null,
						},
					],
				});
				sse(res, {
					...base,
					choices: [
						{
							index: 0,
							delta: {
								tool_calls: [
									{
										index: 0,
										function: { arguments: JSON.stringify({ command: "echo harness-lifecycle-probe" }) },
									},
								],
							},
							finish_reason: null,
						},
					],
				});
				sse(res, { ...base, choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] });
			} else {
				for (const piece of ["Probe ", "complete."]) {
					sse(res, {
						...base,
						choices: [{ index: 0, delta: { role: "assistant", content: piece }, finish_reason: null }],
					});
				}
				sse(res, { ...base, choices: [{ index: 0, delta: {}, finish_reason: "stop" }] });
			}
			res.write("data: [DONE]\n\n");
			res.end();
		}, RESPONSE_DELAY_MS);
	});
});

server.listen(PORT, "127.0.0.1", () => console.log(`fake OpenAI-compatible server on ${PORT}`));
