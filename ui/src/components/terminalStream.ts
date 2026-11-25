/** Upper bound (UTF-16 code units) of output a {@link TerminalStream} retains for replay. */
export const TERMINAL_TAIL_LIMIT = 64 * 1024;

export type TerminalChunkListener = (chunk: string) => void;

/**
 * Bounded pipe from a session's output channel to the terminal view (`05-02`).
 *
 * Chunks go straight to the subscribed terminal (xterm's scrollback is the history bound);
 * React state never holds the output. Only the last {@link TERMINAL_TAIL_LIMIT} characters are
 * kept, to replay into a terminal that subscribes after output has started.
 */
export class TerminalStream {
  private chunks: string[] = [];
  private length = 0;
  private listener: TerminalChunkListener | null = null;

  write(chunk: string): void {
    if (chunk.length === 0) return;
    this.chunks.push(chunk);
    this.length += chunk.length;
    let head = this.chunks[0];
    while (head !== undefined && this.chunks.length > 1 && this.length - head.length >= TERMINAL_TAIL_LIMIT) {
      this.length -= head.length;
      this.chunks.shift();
      head = this.chunks[0];
    }
    // A single oversized chunk; compacting only past 2x keeps this amortised O(1) per write.
    if (this.length > 2 * TERMINAL_TAIL_LIMIT) {
      const kept = this.tail();
      this.chunks = [kept];
      this.length = kept.length;
    }
    this.listener?.(chunk);
  }

  /** The retained tail of the output (at most {@link TERMINAL_TAIL_LIMIT} characters). */
  tail(): string {
    return this.chunks.join("").slice(-TERMINAL_TAIL_LIMIT);
  }

  /** Characters currently held (bounded by `2 * TERMINAL_TAIL_LIMIT`). */
  retainedLength(): number {
    return this.length;
  }

  /** Replays the retained tail to `listener`, then forwards new chunks until unsubscribed. */
  subscribe(listener: TerminalChunkListener): () => void {
    const tail = this.tail();
    if (tail.length > 0) listener(tail);
    this.listener = listener;
    return () => {
      if (this.listener === listener) this.listener = null;
    };
  }
}
