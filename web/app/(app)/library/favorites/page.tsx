import { FavoritesView } from "@/components/favorites-view";

/** 全部收藏（/library/favorites）：与单库页同一套海报墙，首页横滚行只放最近 20 部。 */
export default function FavoritesPage() {
  return (
    <div className="flex h-full flex-col">
      <FavoritesView />
    </div>
  );
}
