import { runInNewContext } from "node:vm";
import { expect, it, vi } from "vitest";
import { encodeWav } from "./voice";
import processor from "./voice-processor.js?raw";

it("flushes captured PCM into the WAV format the Mac expects", async () => {
  const postMessage = vi.fn<(samples: Int16Array) => void>();
  class AudioWorkletProcessor {
    port = { postMessage, onmessage: () => {} };
  }
  type Processor = AudioWorkletProcessor & { process(inputs: Float32Array[][]): boolean };
  const registerProcessor = vi.fn<(name: string, node: new () => Processor) => void>();
  runInNewContext(processor, { AudioWorkletProcessor, registerProcessor, sampleRate: 48_000 });
  const [name, Node] = registerProcessor.mock.calls[0];
  expect(name).toBe("voice-recorder");
  const node = new Node();
  node.process([[Float32Array.of(-1, 0, 1)]]);
  expect(postMessage).not.toHaveBeenCalled();
  node.port.onmessage();
  expect(postMessage.mock.calls[0][0]).toHaveLength(3);

  const wav = encodeWav(postMessage.mock.calls[0][0], 48_000);
  const bytes = await new Promise<ArrayBuffer>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as ArrayBuffer);
    reader.onerror = () => reject(reader.error);
    reader.readAsArrayBuffer(wav);
  });
  const view = new DataView(bytes);
  expect(wav.type).toBe("audio/wav");
  expect(view.getUint32(0)).toBe(0x52494646); // RIFF
  expect(view.getUint32(24, true)).toBe(48_000);
  expect(view.getUint32(40, true)).toBe(6);
  expect(view.getInt16(44, true)).toBe(-32768);
  expect(view.getInt16(48, true)).toBe(32767);
});
