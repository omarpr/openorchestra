// This file runs in the browser's AudioWorklet scope.
class VoiceProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.samples = new Int16Array(Math.floor(sampleRate * 30));
    this.length = 0;
    this.port.onmessage = () => this.finish();
  }

  finish() {
    if (!this.samples) return;
    const pcm = this.samples.subarray(0, this.length);
    this.samples = null;
    this.port.postMessage(pcm, [pcm.buffer]);
  }

  process(inputs) {
    if (!this.samples) return false;
    const channel = inputs[0][0];
    if (!channel) return true;
    const count = Math.min(channel.length, this.samples.length - this.length);
    for (let i = 0; i < count; i++) {
      const sample = Math.max(-1, Math.min(1, channel[i]));
      this.samples[this.length++] = sample < 0 ? sample * 32768 : sample * 32767;
    }
    if (this.length === this.samples.length) this.finish();
    return true;
  }
}

registerProcessor("voice-recorder", VoiceProcessor);
