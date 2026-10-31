import type { FavoriteSortParams } from "@/lib/api/playback";

export interface CursorPage<T> {
  items: T[];
  total: number | null;
  next_cursor: string | null;
}
export interface WindowPage<T> extends CursorPage<T> { start: number; }
export type FavoriteScope = FavoriteSortParams & { unwatched_first?: boolean };
type Loader<T> = (options: FavoriteScope & { limit: number; after?: string }) => Promise<CursorPage<T>>;
type Seek = (options: FavoriteScope & { start: number }) => Promise<{ start: number; after: string | null }>;

export class StaleFavoriteWindow extends Error {
  constructor() { super("Favorite window replaced"); }
}

/** Actual wall and gallery navigation: cursor append, explicit seek for jumps. */
export class FavoriteWindow<T> {
  private generation = 0;
  private scopeKey: string | null = null;
  private readonly boundaries = new Map<number, string>();

  constructor(private readonly load: Loader<T>, private readonly seek: Seek) {}

  invalidate(): void {
    this.generation += 1;
    this.boundaries.clear();
  }

  private scope(scope: FavoriteScope): number {
    const key = JSON.stringify([scope.sort, scope.order, scope.unwatched_first ?? false]);
    if (key !== this.scopeKey) {
      this.invalidate();
      this.scopeKey = key;
    }
    return this.generation;
  }

  private check(generation: number): void {
    if (generation !== this.generation) throw new StaleFavoriteWindow();
  }

  async page(start: number, limit: number, scope: FavoriteScope): Promise<CursorPage<T>> {
    const generation = this.scope(scope);
    let after = start === 0 ? undefined : this.boundaries.get(start);
    if (start > 0 && after === undefined) {
      const boundary = await this.seek({ ...scope, start });
      this.check(generation);
      after = boundary.after ?? undefined;
    }
    const page = await this.load({ ...scope, limit, ...(after ? { after } : {}) });
    this.check(generation);
    if (page.next_cursor) this.boundaries.set(start + page.items.length, page.next_cursor);
    return page;
  }

  async refresh(start: number, count: number, limit: number, scope: FavoriteScope): Promise<WindowPage<T>> {
    this.invalidate();
    const generation = this.scope(scope);
    const head = await this.page(0, limit, scope);
    this.check(generation);
    if (head.total === null) throw new Error("收藏首页缺少总数");
    if (start >= head.total) return { ...head, start: 0 };
    let page = start === 0 ? head : await this.page(start, limit, scope);
    this.check(generation);
    const items = [...page.items];
    while (page.next_cursor && items.length < Math.max(count, limit)) {
      page = await this.page(start + items.length, limit, scope);
      this.check(generation);
      items.push(...page.items);
    }
    return { items, total: head.total, next_cursor: page.next_cursor, start };
  }
}
