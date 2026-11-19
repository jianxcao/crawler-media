/** 设置分区的标识。 */
export type SettingsSectionId =
  | "overview"
  | "profile"
  | "appearance"
  | "members"
  | "subscription"
  | "metadata"
  | "scrape"
  | "sites"
  | "downloaders"
  | "logs";

/** 设置页的分区（进入设置后替换左侧菜单）。 */
export interface SettingsSection {
  id: SettingsSectionId;
  label: string;
  description: string;
}

/** 设置分区的分组：侧栏按组渲染小节标题，组内顺序即展示顺序。 */
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
      },
    ],
  },
  {
    label: "账号",
    items: [
      { id: "profile", label: "个人信息", description: "头像、昵称与登录密码" },
      { id: "appearance", label: "外观", description: "界面主题与玻璃质感" },
    ],
  },
  {
    label: "成员",
    items: [
      { id: "members", label: "成员", description: "家庭成员账号、能力开关与可见范围" },
    ],
  },
  {
    label: "资源与下载",
    items: [
      { id: "subscription", label: "订阅规则", description: "订阅规则组与投递模拟预演" },
      { id: "metadata", label: "元数据源", description: "TMDB Key、自动封面与图片刷新策略" },
      { id: "scrape", label: "刮削与整理", description: "元数据语言、选图偏好、命名模板与目录写入" },
      { id: "sites", label: "资源站点", description: "站点接入与鉴权、搜索分类" },
      { id: "downloaders", label: "下载器", description: "qBittorrent / Transmission 接入" },
    ],
  },
  {
    label: "系统",
    items: [
      { id: "logs", label: "系统日志", description: "查看实时运行日志、下载调度与错误排查" },
    ],
  },
];

/** 扁平分区列表：路由校验、默认分区等按 id 消费的场景继续用它。 */
export const settingsSections: SettingsSection[] = settingsSectionGroups.flatMap(
  (group) => group.items,
);

/** 成员可见的设置分区（其余分区后端一律 403，前端不给入口）。 */
const MEMBER_SECTION_IDS = new Set<SettingsSectionId>(["profile", "appearance"]);

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
