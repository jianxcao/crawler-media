/** Reserve browser connections for API/navigation traffic on HTTP/1.1 origins. */
export class ArtworkQueue {
  private active = 0;
  private pending: Array<{ priority: number; start: () => void; cancelled: boolean }> = [];

  enqueue(start: () => void, priority: number) {
    const entry = { priority, start, cancelled: false };
    let running = false;
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      entry.cancelled = true;
      if (running) this.active--;
      this.drain();
    };
    entry.start = () => { running = true; start(); };
    this.pending.push(entry);
    this.drain();
    return {
      finish,
      prioritize: (value: number) => { entry.priority = value; },
    };
  }

  private drain() {
    this.pending = this.pending.filter((entry) => !entry.cancelled);
    this.pending.sort((left, right) => left.priority - right.priority);
    while (this.active < 4 && this.pending.length) {
      const entry = this.pending.shift()!;
      this.active++;
      entry.start();
    }
  }
}

export const artworkQueue = new ArtworkQueue();
