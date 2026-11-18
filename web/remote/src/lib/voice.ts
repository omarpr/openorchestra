import processorUrl from "./voice-processor.js?url&no-inline";

const RECORDING_MS = 30_000;

export function canRecordVoice() {
  return typeof AudioWorkletNode !== "undefined" && typeof navigator.mediaDevices?.getUserMedia === "function";
}

export function encodeWav(samples: Int16Array, rate: number): Blob {
  const bytes = new ArrayBuffer(44 + samples.length * 2);
  const view = new DataView(bytes);
  const label = (offset: number, tag: string) => {
    for (let i = 0; i < tag.length; i++) view.setUint8(offset + i, tag.charCodeAt(i));
  };
  label(0, "RIFF");
  view.setUint32(4, bytes.byteLength - 8, true);
  label(8, "WAVEfmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  label(36, "data");
  view.setUint32(40, samples.length * 2, true);
  for (let i = 0; i < samples.length; i++) view.setInt16(44 + i * 2, samples[i], true);
  return new Blob([bytes], { type: "audio/wav" });
}

function capture(
  context: AudioContext,
  stream: MediaStream,
  signal: AbortSignal,
  onRecording: (stop: () => void) => void,
): Promise<Int16Array> {
  const source = context.createMediaStreamSource(stream);
  const recorder = new AudioWorkletNode(context, "voice-recorder", {
    channelCount: 1,
    channelCountMode: "explicit",
  });
  source.connect(recorder);
  // The worklet writes no output. Connecting it keeps the audio graph running.
  recorder.connect(context.destination);
  return new Promise((resolve, reject) => {
    let stopTimeout: ReturnType<typeof setTimeout> | undefined;
    const stop = () => {
      if (stopTimeout !== undefined) return;
      recorder.port.postMessage("stop");
      // Wait for the worklet to flush before disconnecting the input graph.
      stopTimeout = setTimeout(() => end(new Error("Recording interrupted")), 1000);
    };
    const maxDuration = setTimeout(stop, RECORDING_MS);
    const end = (result: Int16Array | Error) => {
      clearTimeout(maxDuration);
      clearTimeout(stopTimeout);
      signal.removeEventListener("abort", abort);
      recorder.port.close();
      if (result instanceof Error) reject(result);
      else resolve(result);
    };
    const abort = () => end(new DOMException("Recording canceled", "AbortError"));
    signal.addEventListener("abort", abort, { once: true });
    recorder.onprocessorerror = () => end(new Error("Recording interrupted"));
    recorder.port.onmessage = (event: MessageEvent<Int16Array>) => end(event.data);
    onRecording(stop);
  });
}

export async function recordVoice(signal: AbortSignal, onRecording: (stop: () => void) => void): Promise<Blob> {
  // Resume while the tap still counts as a user gesture on iOS.
  const context = new AudioContext();
  const resumed = context.resume();
  void resumed.catch(() => {});
  let stream: MediaStream | undefined;
  const release = () => {
    for (const track of stream?.getTracks() ?? []) track.stop();
    if (context.state !== "closed") void context.close().catch(() => {});
  };
  signal.addEventListener("abort", release, { once: true });
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    signal.throwIfAborted();
    await resumed;
    await context.audioWorklet.addModule(processorUrl);
    signal.throwIfAborted();
    const samples = await capture(context, stream, signal, onRecording);
    if (!samples.length) throw new Error("No audio recorded");
    return encodeWav(samples, context.sampleRate);
  } finally {
    signal.removeEventListener("abort", release);
    release();
  }
}
