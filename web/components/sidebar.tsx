"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

import {
  ActivityIcon,
  BookmarkIcon,
  ClockIcon,
  DatabaseIcon,
  FolderSyncIcon,
  LibraryIcon,
  MoreIcon,
  PanelLeftIcon,
  PlusIcon,
} from "@/components/icons";
import { useToast } from "@/components/feedback";
import { JobCenter } from "@/components/job-center";
import { UserMenu } from "@/components/user-menu";
import { useBackdrop } from "@/lib/backdrop";
import { sidebarGlass } from "@/lib/glass";
import { applyNavOrder } from "@/lib/sidebar-nav";
import { useUiPrefs } from "@/lib/ui-prefs";
import { useIsMobile } from "@/lib/use-media-query";
import { publicEnv } from "@/lib/env";
import { useSession } from "@/lib/session";
import { usePermissions } from "@/lib/permissions";
import { exploreItems } from "@/lib/mock-data";
import { GlassPanel } from "@/components/glass-panel";
import { SearchCommand, type SearchSubmitOptions } from "@/components/search-command";
import type { SearchScope } from "@/lib/categories";

/**
 * 工作台侧边栏。结构（自上而下，对齐 ChatGPT Codex 侧栏的极简版式）：
 *   品牌头部（右侧搜索图标，⌘K）→ 扁平主导航 → 「最近会话」分组 → 左下角用户信息。
 * 面板本体用真实 WebGL 液态玻璃（GlassPanel, dark 预设）承载，透出并折射背景大图，
 * 是整屏的视觉主角。
 *
 * 折叠形态（collapsed）：只留图标的窄玻璃条——品牌徽标 / 开关 / 搜索竖排在头部，
 * 主导航仅显示居中图标（title 提示文案），「最近会话」整组隐藏，左下角只留头像。
 * 宽度动画由外层 app-shell 的 aside 承担，本组件只负责两种版式的内容切换。
 */
export interface SidebarProps {
  activeNav: string;
  onSelect: (id: string) => void;
  onSearch: (keyword: string, scope: SearchScope, options?: SearchSubmitOptions) => void;
  onOpenSettings: (sectionId?: string) => void;
  /** 是否处于折叠（图标窄栏）形态 */
  collapsed: boolean;
  /** 点击头部开关按钮：在展开 / 折叠间切换 */
  onToggleCollapse: () => void;
  /** 实色形态：沉浸页（Agent 对话）用——不渲染 WebGL 玻璃、不折射背景大图 */
  flat?: boolean;
}

/** 主导航：新会话 / 媒体库 / 探索项 / 订阅 / 活动，合并成一列扁平列表。
 *  「新会话」是 Agent 入口（管理员专属，成员侧隐藏——后端也会 403）。
 *
 *  这个数组的次序就是**内置默认顺序**：用户在「设置 → 外观 → 导航顺序」里排过
 *  的项按个人顺序在前，没排过的（包括版本升级新增的入口）按这里的次序追加在后
 *  （合并规则见 lib/sidebar-nav.ts）。设置页复用同一份清单渲染排序列表，
 *  两边的图标与文案不会各写一份。 */
export const SIDEBAR_NAV_ITEMS = [
  { id: "library", label: "媒体库", icon: LibraryIcon },
  { id: "collections", label: "合集", icon: LibraryIcon },
  ...exploreItems,
  { id: "subscriptions", label: "我的订阅", icon: BookmarkIcon },
  { id: "transfers", label: "媒体整理", icon: FolderSyncIcon },
  { id: "jobs", label: "定时任务", icon: ClockIcon },
  { id: "cache", label: "元数据缓存", icon: DatabaseIcon },
  { id: "tasks", label: "活动", icon: ActivityIcon },
];

const memberNavItems = SIDEBAR_NAV_ITEMS;


/**
 * 当前用户实际可见的主导航项（未排序）。侧栏与设置页的排序列表必须用同一套
 * 可见性判定，否则会出现"设置页能排、侧栏没有"这种对不上的项。
 */
export function useVisibleNavItems() {
  const { session } = useSession();
  const { isAdmin, canSubscribe } = usePermissions();
  const items = session.role === "member" ? memberNavItems : SIDEBAR_NAV_ITEMS;
  return items.filter((item) => {
    if (item.id === "subscriptions") return canSubscribe;
    // 活动页与任务数据都是管理员专属（JobCenter 自身也有这道判断，成员侧渲染为空），
    // 这里必须同样挡掉，否则设置页会列出一条侧栏根本没有的可排序项
    if (item.id === "tasks" || item.id === "transfers" || item.id === "jobs" || item.id === "cache") return isAdmin;
    return true;
  });
}

export function Sidebar({
  activeNav,
  onSelect,
  onSearch,
  onOpenSettings,
  collapsed,
  onToggleCollapse,
  flat = false,
}: SidebarProps) {
  const { backdrop } = useBackdrop();
  // 移动端的搜索入口在全局顶栏上（常驻可见），侧栏里这颗就是重复的。
  // 必须条件渲染而不是 CSS 隐藏：SearchCommand 自带全局 ⌘K 监听，
  // 挂两份会让一次快捷键把面板开了又关。
  const isMobile = useIsMobile();
  // 透明度/明暗/厚度来自「设置 → 外观」的用户偏好（ui.preferences.sidebar），
  // 基底为 LiquidGlassCard 同款材质；设置页拖动滑杆时经预览草稿实时生效。
  const { prefs } = useUiPrefs();
  const glass = sidebarGlass(prefs.sidebar);
  // 成员形态做减法：隐藏 Agent 入口（新会话）与「最近会话」组；
  // 这是界面裁剪，安全边界在后端 require_admin
  const { session } = useSession();
  const { canSearch } = usePermissions();
  const isMember = session.role === "member";
  // 个人排序：prefs 已含设置页的未保存草稿，因此在设置页拖动时这里即时跟随
  const visibleNavItems = useVisibleNavItems();
  const navItems = applyNavOrder(visibleNavItems, prefs.nav.order);
  const body = (
    <>
      {/* 品牌头部。展开：完整字标 + 开合/搜索图标横排；折叠：独立徽标、开合、搜索竖排居中。
          开合按钮与搜索共用同一套图标按钮样式（⌘K 在两种形态下均可唤起搜索）。
          两种形态的 logo 都是「回首页」入口，等同于点击导航里的「新会话」。 */}
      {collapsed ? (
        <div className="flex flex-col items-center gap-2 px-3 pb-3 pt-5">
          <BrandHome onSelect={onSelect} homeId={isMember ? "library" : "new"}>
            <img
              src="/logo-mark.svg"
              alt={publicEnv.appName}
              width={32}
              height={32}
              className="size-8 object-contain drop-shadow-[0_2px_8px_rgba(236,72,153,0.3)]"
            />
          </BrandHome>
          <CollapseToggle collapsed onClick={onToggleCollapse} />
          {canSearch && <SearchCommand onSearch={onSearch} />}
        </div>
      ) : (
        <div className="flex items-center justify-between px-4 pb-3 pt-4">
          <BrandHome onSelect={onSelect} homeId={isMember ? "library" : "new"}>
            <div className="flex items-center gap-2.5">
              <img
                src="/logo-mark.svg"
                alt=""
                width={30}
                height={30}
                className="size-[30px] shrink-0 object-contain drop-shadow-[0_2px_8px_rgba(236,72,153,0.3)]"
              />
              <span className="font-semibold tracking-[-0.03em] text-white text-[17px] font-sans">
                Crawler<span className="text-transparent bg-clip-text bg-gradient-to-r from-pink-500 to-orange-400 font-bold ml-1">Media</span>
              </span>
            </div>
          </BrandHome>
          <div className="flex items-center gap-1">
            {!isMobile && canSearch && <SearchCommand onSearch={onSearch} />}
            <CollapseToggle collapsed={false} onClick={onToggleCollapse} />
          </div>
        </div>
      )}

      {/* 导航区。两段式：主导航固定在上方，「最近会话」占满剩余高度并自带
          滚动条——会话可以有几百条，与主导航共用一条滚动条时翻会话会把
          导航推出视野。min-h-0 是 flex 子项能真正收缩、把滚动交给内层的前提。 */}
      <nav className="flex min-h-0 flex-1 flex-col px-3 pb-2">
        {/* 主导航：无分组标题、无图标底片的扁平列表（对齐 Codex 侧栏）；
            折叠时只留居中图标，文案降级为 title 悬浮提示。
            自带 overflow 是矮窗口下的安全阀：空间不够时它自己滚，
            而不是把「最近会话」挤没。 */}
        <div className="scroll-thin space-y-0.5 overflow-y-auto">
          {navItems.map((item) => {
            // 「活动」自带角标与动态落点，交给 JobCenter 渲染；它同样参与个人排序，
            // 所以位置由这里的 map 决定，而不再被钉在主导航末尾
            if (item.id === "tasks") {
              return (
                <JobCenter key={item.id} collapsed={collapsed} active={activeNav === "tasks"} />
              );
            }
            const Icon = item.icon;
            return (
              <button
                key={item.id}
                type="button"
                data-active={activeNav === item.id}
                onClick={() => onSelect(item.id)}
                title={collapsed ? item.label : undefined}
                className={`glass-row nav-item py-2 max-md:py-2.5 ${collapsed ? "justify-center px-0" : "px-3"}`}
              >
                {/* 图标移动端提到 22px：与顶栏图标键同标准（iOS 列表行图标的惯用比例） */}
                <Icon className="size-[18px] shrink-0 max-md:size-[22px]" />
                {/* 字号写在 span 上而非 button 上：globals.css 的 `button { font: inherit }`
                    是无 @layer 的普通规则，会压过 Tailwind 的 @layer utilities——写在
                    button 上的 text-ui 不生效，会退回 body 的 14px。 */}
                {!collapsed && (
                  <span className="flex-1 text-ui font-medium">{item.label}</span>
                )}
              </button>
            );
          })}
        </div>

      </nav>

      {/* 左下角：用户信息（无分割线，靠间距区隔）；折叠时只留头像 */}
      <div className="p-2.5 pt-1.5">
        <UserMenu onOpenSettings={onOpenSettings} collapsed={collapsed} />
      </div>
    </>
  );

  if (flat) {
    // 沉浸页的实色侧栏：只保留浮起卡片的形状语言，不透玻璃
    return <div className="panel--sidebar-flat flex h-full flex-col">{body}</div>;
  }
  return (
    <GlassPanel
      backgroundImage={backdrop}
      variant={glass.variant}
      className="panel--sidebar h-full"
      contentClassName="flex h-full flex-col"
      sampleBackground={glass.sampleBackground}
      settings={glass.settings}
      hairlineAlpha={glass.hairlineAlpha}
      fallbackAlpha={glass.fallbackAlpha}
    >
      {body}
    </GlassPanel>
  );
}

/** 头部的侧栏开合按钮：与搜索触发器同款的方形图标按钮 */
/**
 * 品牌 logo 的「回首页」外壳：点击等同于选中导航里的「新会话」（id: new），
 * 展开态包字标、折叠态包徽标，两处共用同一交互与 hover 反馈。
 */
function BrandHome({
  onSelect,
  homeId,
  children,
}: {
  onSelect: (id: string) => void;
  /** 「回首页」的落点：管理员是新会话页，成员是媒体库（成员没有 Agent 入口） */
  homeId: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={() => onSelect(homeId)}
      aria-label="回到首页"
      title="回到首页"
      className="shrink-0 cursor-pointer transition-opacity hover:opacity-75"
    >
      {children}
    </button>
  );
}

function CollapseToggle({ collapsed, onClick }: { collapsed: boolean; onClick: () => void }) {
  const label = collapsed ? "展开侧栏" : "收起侧栏";
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      // 移动端 44px：这颗键在抽屉里就是「收起抽屉」，iOS HIG 的最小可点目标；
      // 桌面保持 32px 紧凑图标键
      className="glass-row !size-8 shrink-0 justify-center !p-0 max-md:!size-11"
    >
      <PanelLeftIcon className="size-[18px] max-md:size-[22px]" />
    </button>
  );
}

/** 触底预加载的提前量：距底部还有这么多像素就取下一页，滚到底时数据已就位。 */
const LOAD_MORE_THRESHOLD_PX = 120;
