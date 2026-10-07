import { Outlet } from "react-router-dom";
import { AppShell } from "@/components/app-shell";
import { AuthGate } from "@/components/auth-gate";
import { DownloadTasksProvider } from "@/lib/download-tasks";
import { JobsProvider } from "@/lib/jobs";

/**
 * (app) 路由组：工作台全部页面共用的外壳布局。
 * AuthGate 在确认登录状态前不渲染工作台，避免未登录时闪现主界面再跳转；
 * AppShell 提供两栏骨架（侧栏 + 主区），右区经 <Outlet/> 渲染当前路由页面。
 * /login、/setup 在组外，不套外壳。
 */
export default function AppLayout() {
  return (
    <AuthGate>
      <JobsProvider>
        <DownloadTasksProvider>
          <AppShell>
            <Outlet />
          </AppShell>
        </DownloadTasksProvider>
      </JobsProvider>
    </AuthGate>
  );
}
