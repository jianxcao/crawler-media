/**
 * SPA 兜底 404:react-router 没有 notFound(),未匹配路径统一渲染这里。
 * 各页面里的 notFound() 调用也改为返回 <NotFound/>。
 */
export function NotFound() {
  return (
    <div className="relative z-10 flex h-dvh w-full flex-col items-center justify-center gap-2 bg-[var(--bg)] text-[var(--text)]">
      <div className="text-5xl font-semibold">404</div>
      <div className="text-[var(--text-muted)]">页面不存在或已被移动</div>
    </div>
  );
}
