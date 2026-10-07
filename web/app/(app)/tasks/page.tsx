import { Navigate, useSearchParams } from "react-router-dom";

/**
 * 旧的任务中心路径。页面已重构为「活动」（观看 + 任务两个视角），此处永久
 * 重定向，保住用户收藏与站内历史深链——`?view=active` 这类查询原样透传，
 * 迁移后仍精确落在任务视角的对应状态。
 */
export default function TasksRedirectPage() {
  const [searchParams] = useSearchParams();
  const suffix = searchParams.toString();
  return <Navigate to={`/activity${suffix ? `?${suffix}` : ""}`} replace />;
}
