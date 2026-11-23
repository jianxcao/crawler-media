import { Suspense, useEffect } from "react";
import { Outlet } from "react-router-dom";
import { AuthGate } from "@/components/auth-gate";
import { RouteFallback } from "@/components/route-fallback";

/**
 * 播放路由组：**刻意不套工作台外壳**。
 *
 * 播放器要独占整块画面，侧栏、顶栏、背景大图在这里全是干扰；全屏时它们还会
 * 与系统控件打架。所以 /play 与 /login、/setup 一样留在 (app) 组之外，只保留
 * 登录闸门。挂载期间固定浏览器 UI 框颜色为纯黑，卸载时还原进入前的主题画布色。
 */
export default function PlayLayout() {
  useEffect(() => {
    const meta = document.querySelector('meta[name="theme-color"]');
    if (!meta) return;
    const previous = meta.getAttribute("content");
    meta.setAttribute("content", "#000000");
    return () => {
      if (previous !== null) {
        meta.setAttribute("content", previous);
      }
    };
  }, []);

  return (
    <AuthGate>
      <div className="h-dvh w-full overflow-hidden bg-black">
        <Suspense fallback={<RouteFallback />}>
          <Outlet />
        </Suspense>
      </div>
    </AuthGate>
  );
}
