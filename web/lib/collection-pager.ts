import type { CollectionItem, CollectionItemPage } from "@/lib/api/collections";

export interface CollectionPageState {
  items: CollectionItem[] | null;
  total: number | null;
  nextCursor: string | null;
  loading: boolean;
  error: string | null;
}

type LoadPage = (id: string, options: { limit: number; cursor?: string }) => Promise<CollectionItemPage>;
const emptyState = (): CollectionPageState => ({
  items: null, total: null, nextCursor: null, loading: false, error: null,
});

/** Request state for collection detail, including route changes and removal refreshes. */
export class CollectionPager {
  state: CollectionPageState = emptyState();
  private id: string | null = null;
  private generation = 0;
  private readonly listeners = new Set<(state: CollectionPageState) => void>();

  constructor(private readonly load: LoadPage) {}

  subscribe(listener: (state: CollectionPageState) => void): () => void {
    this.listeners.add(listener);
    listener(this.state);
    return () => { this.listeners.delete(listener); };
  }

  private publish(state: CollectionPageState): void {
    this.state = state;
    for (const listener of this.listeners) listener(state);
  }

  clear(): void {
    this.generation += 1;
    this.id = null;
    this.publish(emptyState());
  }

  async reset(id: string): Promise<void> {
    this.generation += 1;
    this.id = id;
    this.publish(emptyState());
    await this.request();
  }

  async refresh(expectedId?: string): Promise<void> {
    if (this.id && (!expectedId || expectedId === this.id)) await this.reset(this.id);
  }

  async loadMore(): Promise<void> {
    if (!this.state.loading && this.state.nextCursor) await this.request(this.state.nextCursor);
  }

  private async request(cursor?: string): Promise<void> {
    if (!this.id) return;
    const generation = this.generation;
    const id = this.id;
    this.publish({ ...this.state, loading: true, error: null });
    try {
      const page = await this.load(id, { limit: 40, ...(cursor ? { cursor } : {}) });
      if (generation !== this.generation) return;
      const existing = cursor ? this.state.items ?? [] : [];
      const seen = new Set(existing.map((item) => item.media_item_id));
      const incoming = page.items.filter((item) => {
        if (seen.has(item.media_item_id)) return false;
        seen.add(item.media_item_id);
        return true;
      });
      this.publish({
        items: [...existing, ...incoming],
        total: page.total,
        nextCursor: page.next_cursor,
        loading: false,
        error: null,
      });
    } catch (error) {
      if (generation !== this.generation) return;
      this.publish({
        ...this.state,
        loading: false,
        error: error instanceof Error ? error.message : "加载失败",
      });
    }
  }
}
