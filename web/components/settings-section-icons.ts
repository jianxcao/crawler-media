import type { ComponentType, SVGProps } from "react";

import {
  ActivityIcon,
  BookmarkIcon,
  DownloadIcon,
  PaletteIcon,
  PhotoIcon,
  PlugIcon,
  ServerIcon,
  ShieldIcon,
  TerminalIcon,
  UserIcon,
  WandIcon,
} from "@/components/icons";
import type { SettingsSectionId } from "@/lib/settings-sections";

type IconComponent = ComponentType<SVGProps<SVGSVGElement>>;

/** 设置分区图标映射：与纯配置元数据（lib/settings-sections.ts）解耦。 */
export const SETTINGS_SECTION_ICON: Record<SettingsSectionId, IconComponent> = {
  overview: ActivityIcon,
  profile: UserIcon,
  appearance: PaletteIcon,
  members: ShieldIcon,
  subscription: BookmarkIcon,
  metadata: PhotoIcon,
  scrape: WandIcon,
  sites: ServerIcon,
  downloaders: DownloadIcon,
  plugins: PlugIcon,
  logs: TerminalIcon,
};
