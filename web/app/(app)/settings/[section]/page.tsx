import { Navigate, useParams, useSearchParams } from "react-router-dom";

import { SettingsPanel } from "@/components/settings-view";
import { NotFound } from "@/src/not-found";
import { settingsSections } from "@/lib/mock-data";

/** 设置分区（/settings/[section]）：概览 / 个人信息 / 外观 / 站点 / 下载器 等。 */
export default function SettingsSectionPage() {
  const { section = "" } = useParams();
  const [searchParams] = useSearchParams();
  // 旧「搜索」分区已并入「资源站点」，老书签/历史链接重定向过去，不要 404
  if (section === "search") return <Navigate to="/settings/sites" replace />;
  // 旧「关于与更新」分区已并入「应用」（现「更新与维护」），同样重定向兜底
  if (section === "about") return <Navigate to="/settings/app" replace />;
  // 旧「应用 → 远程转码」标签已升级为「播放」分区，带 tab=remote 的老链接跟过去
  if (section === "app" && searchParams.get("tab") === "remote") {
    return <Navigate to="/settings/playback" replace />;
  }
  if (!settingsSections.some((s) => s.id === section)) return <NotFound />;
  return <SettingsPanel active={section} />;
}
