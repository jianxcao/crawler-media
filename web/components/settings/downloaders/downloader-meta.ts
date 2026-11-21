import type { DownloaderClientType, DownloaderStatus, PathProbe } from "@/lib/api/downloaders";

/** 连接状态 → 展示文案与颜色（与站点配置同语言） */
export const STATUS_META: Record<DownloaderStatus, { label: string; color: string }> = {
  active: { label: "已连接", color: "var(--ok)" },
  verifying: { label: "测试中", color: "var(--info)" },
  pending: { label: "待测试", color: "#c0c4cc" },
  unverified: { label: "未验证", color: "#c0c4cc" },
  disabled: { label: "已停用", color: "var(--text-faint)" },
  failed: { label: "连接失败", color: "var(--danger)" },
};

/** 下载器类型 → 展示名 */
export const TYPE_LABEL: Record<DownloaderClientType, string> = {
  qbittorrent: "qBittorrent",
  transmission: "Transmission",
};

/** 各类型的地址占位提示（qB 是 WebUI 地址，Tr 是 RPC 地址，端口不同） */
export const URL_PLACEHOLDER: Record<DownloaderClientType, string> = {
  qbittorrent: "http://192.168.1.10:8080",
  transmission: "http://192.168.1.10:9091",
};

/** 路径体检状态 → 展示。ok 不额外标注（正常不该抢注意力），异常各给一个短词 */
export const PATH_STATE_LABEL: Record<Exclude<PathProbe["state"], "ok">, string> = {
  empty: "目录为空",
  not_dir: "不是目录",
  missing: "不存在",
  unmapped: "未覆盖",
};
