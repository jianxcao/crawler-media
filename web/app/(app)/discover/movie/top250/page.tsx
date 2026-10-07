import { Link } from "react-router-dom";

/** 兼容旧书签：明确提示旧榜单调整，并提供豆瓣高分榜单链接。 */
export default function Top250Page() {
  return (
    <div className="flex h-full flex-col items-center justify-center p-6 text-center">
      <h2 className="text-xl font-bold text-white">榜单路径调整提示</h2>
      <p className="mt-2 text-ui text-[var(--text-muted)]">
        原「Top250」旧榜单已升级归并为「豆瓣高分」稳定片单。
      </p>
      <Link
        to="/discover/movie/collections/douban/top-rated"
        replace
        className="btn-accent mt-5 inline-flex h-9 items-center rounded-full px-5 text-ui font-semibold"
      >
        前往「豆瓣高分」片单 →
      </Link>
    </div>
  );
}
