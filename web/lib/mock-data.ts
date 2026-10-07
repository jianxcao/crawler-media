/**
 * 骨架阶段的占位数据。
 * 后续接入 FastAPI 后端时，这里的静态数组会替换为接口返回（见 lib/api/）。
 * 现在只为把布局与交互跑通，字段尽量贴近真实产品形态。
 */
import type { ComponentType, SVGProps } from "react";
import {
  ActivityIcon,
  BookmarkIcon,
  ChatIcon,
  DeviceIcon,
  DownloadIcon,
  WandIcon,
  FilmIcon,
  FolderIcon,
  GearIcon,
  GlobeIcon,
  PaletteIcon,
  PhotoIcon,
  PlayIcon,
  SendIcon,
  ServerIcon,
  ShieldIcon,
  SparkIcon,
  TerminalIcon,
  TvIcon,
  UserIcon,
  PlugIcon,
} from "@/components/icons";

type Icon = ComponentType<SVGProps<SVGSVGElement>>;

/** 「探索」分组里的操作按钮 */
export interface ExploreItem {
  id: string;
  label: string;
  icon: Icon;
}

export const exploreItems: ExploreItem[] = [
  { id: "explore-movies", label: "发现电影", icon: FilmIcon },
  { id: "explore-tv", label: "发现剧集", icon: TvIcon },
];

/** 最近会话（类 Codex / ChatGPT 的会话列表） */
export type RunStatus = "running" | "done" | "failed";

export interface RecentRun {
  id: string;
  title: string;
  preview: string;
  status: RunStatus;
  time: string;
}

export const recentRuns: RecentRun[] = [
  {
    id: "run-1",
    title: "追踪《沙丘 2》4K 资源",
    preview: "已在 3 个站点命中，正在校验做种健康度…",
    status: "running",
    time: "刚刚",
  },
  {
    id: "run-2",
    title: "订阅《幕府将军》全季",
    preview: "第 10 集已入库，等待字幕匹配。",
    status: "done",
    time: "12 分钟前",
  },
  {
    id: "run-3",
    title: "补齐《绝命毒师》缺失剧集",
    preview: "S02E07 未找到合适资源。",
    status: "failed",
    time: "1 小时前",
  },
  {
    id: "run-4",
    title: "每周热门电影自动巡检",
    preview: "已生成 8 条候选，全部确认入库。",
    status: "done",
    time: "昨天",
  },
  {
    id: "run-5",
    title: "订阅《怪奇物语》最终季",
    preview: "全季 8 集已入库，字幕匹配完成。",
    status: "done",
    time: "2 天前",
  },
  {
    id: "run-6",
    title: "清理低做种历史种子",
    preview: "已归档 23 个种子，释放 180GB。",
    status: "done",
    time: "3 天前",
  },
];

export const runStatusMeta: Record<RunStatus, { label: string; color: string }> = {
  running: { label: "运行中", color: "var(--info)" },
  done: { label: "已完成", color: "var(--ok)" },
  failed: { label: "失败", color: "var(--danger)" },
};

/** 设置页的分区（进入设置后替换左侧菜单） */
export interface SettingsSection {
  id: string;
  label: string;
  description: string;
  icon: Icon;
}

/** 设置分区的分组：侧栏按组渲染小节标题，组内顺序即展示顺序 */
export interface SettingsSectionGroup {
  label: string;
  items: SettingsSection[];
}

/**
 * 分组标准统一为「回答用户什么问题」：账号（我是谁）→ 成员与设备（谁能进来）→
 * 资源与下载（内容怎么来）→ 媒体库（内容长什么样、怎么看）→ 通知与集成
 * （系统怎么告诉外界）→ 系统（运维）。文件到手之前的事归「资源与下载」，
 * 到手之后归「媒体库」，这条边界决定了刮削、播放的归属。
 */
export const settingsSectionGroups: SettingsSectionGroup[] = [
  {
    label: "",
    items: [
      {
        id: "overview",
        label: "概览",
        description: "配置状态一览：缺什么、有什么问题、下一步做什么",
        icon: ActivityIcon,
      },
    ],
  },
  {
    label: "账号",
    items: [
      { id: "profile", label: "个人信息", description: "头像、昵称与登录密码", icon: UserIcon },
      { id: "appearance", label: "外观", description: "界面主题与玻璃质感", icon: PaletteIcon },
    ],
  },
  {
    label: "成员",
    items: [
      { id: "members", label: "成员", description: "家庭成员账号、能力开关与可见范围", icon: ShieldIcon },
    ],
  },
  {
    label: "资源与下载",
    items: [
      { id: "subscription", label: "订阅规则", description: "订阅规则组与投递模拟预演", icon: BookmarkIcon },
      { id: "metadata", label: "元数据源", description: "TMDB Key、自动封面与图片刷新策略", icon: PhotoIcon },
      { id: "scrape", label: "刮削与整理", description: "元数据语言、选图偏好、命名模板与目录写入", icon: WandIcon },
      { id: "sites", label: "资源站点", description: "站点接入与鉴权、搜索分类", icon: ServerIcon },
      { id: "downloaders", label: "下载器", description: "qBittorrent / Transmission 接入", icon: DownloadIcon },
    ],
  },
  {
    label: "系统",
    items: [
      { id: "logs", label: "系统日志", description: "查看实时运行日志、下载调度与错误排查", icon: TerminalIcon },
    ],
  },
];

/** 扁平分区列表：路由校验、默认分区等按 id 消费的场景继续用它 */
export const settingsSections: SettingsSection[] = settingsSectionGroups.flatMap(
  (group) => group.items,
);

/** 成员可见的设置分区（其余分区后端一律 403，前端不给入口）。 */
const MEMBER_SECTION_IDS = new Set(["profile", "appearance"]);

/**
 * 按角色过滤设置分区分组：管理员全量；成员只剩「账号」组的个人分区
 * （概览呈现的是全局配置健康，属管理员视角，成员不可见）。
 * 这只是界面裁剪——安全边界在后端的 require_admin / 守护测试。
 */
export function settingsSectionGroupsFor(
  role: "admin" | "member",
): SettingsSectionGroup[] {
  if (role === "admin") return settingsSectionGroups;
  return settingsSectionGroups
    .map((group) => ({
      ...group,
      items: group.items.filter((s) => MEMBER_SECTION_IDS.has(s.id)),
    }))
    .filter((group) => group.items.length > 0);
}
