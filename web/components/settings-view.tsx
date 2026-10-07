"use client";

import { useNavigate } from "react-router-dom";
import { useEffect, useState } from "react";

import { AvatarBadge } from "@/components/avatar-badge";
import { DownloaderConfigSection } from "@/components/downloader-config-section";
import { useConfirm, useToast } from "@/components/feedback";
import { SettingsOverviewSection } from "@/components/settings-overview-section";
import { MembersSection } from "@/components/members-section";
import { MetadataSettingsSection } from "@/components/metadata-settings-section";
import { ScrapeSettingsSection } from "@/components/scrape-settings-section";
import { SiteConfigSection, SitesSectionSubtitle } from "@/components/site-config-section";
import { SubscriptionSettingsSection } from "@/components/subscription-settings-section";
import { SystemLogsSection } from "@/components/system-logs-section";
import { GlassPanel } from "@/components/glass-panel";
import { ArrowLeftIcon } from "@/components/icons";
import { useBackdrop } from "@/lib/backdrop";
import { sidebarGlass } from "@/lib/glass";
import { clearPlaybackHistory } from "@/lib/api/playback";
import { useSession } from "@/lib/session";
import { settingsSectionGroupsFor, settingsSections } from "@/lib/mock-data";
import { useUiPrefs } from "@/lib/ui-prefs";
import { THEMES } from "@/lib/themes";

/**
 * 设置模式的左栏：替换掉工作台侧边栏。
 * 顶部是「返回」按钮（回到主区域 / 工作台），下面是设置分区列表。
 */
export interface SettingsSidebarProps {
  active: string;
  onSelect: (id: string) => void;
  onBack: () => void;
}

export function SettingsSidebar({ active, onSelect, onBack }: SettingsSidebarProps) {
  const { backdrop } = useBackdrop();
  // 成员只看到「通用」组（个人信息）；管理分区后端一律 403，前端不给入口
  const { session } = useSession();
  const sectionGroups = settingsSectionGroupsFor(session.role);
  // 与工作台侧栏共用同一份用户偏好（透明度/明暗）；外观分区拖动滑杆时，
  // 这块面板就是实时预览的对象。
  const { prefs } = useUiPrefs();
  const glass = sidebarGlass(prefs.sidebar);
  // 有可用更新时给「更新与维护」分区行点一颗小蓝点：从别的入口进了设置，
  // 也能一眼看出更新在哪一栏（与侧栏更新入口同一份快照数据）
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
      {/* 顶部：返回按钮（玻璃胶囊，轮廓明确的「可点」入口） */}
      <div className="px-3 pb-3 pt-3.5">
        <button
          type="button"
          onClick={onBack}
          className="btn-glass px-3.5 py-1.5 text-sub font-medium text-[var(--text-muted)] hover:text-[var(--text)]"
        >
          <ArrowLeftIcon className="size-4" />
          <span>返回工作台</span>
        </button>
      </div>

      {/* 分区列表：标准 SaaS 设置菜单——紧凑单行（小图标内联 + 标签），
          描述只在右侧面板头部展示，选中态仍用亮胶囊表达 */}
      <nav className="scroll-thin flex-1 space-y-4 overflow-y-auto px-3 pb-4">
        {sectionGroups.map((group) => (
          // 概览组不设标题（label 为空串），空标题不渲染小节头；key 落到首个分区 id
          <div key={group.label || group.items[0]?.id}>
            {group.label && <h3 className="group-label mb-1.5 px-2">{group.label}</h3>}
            <div className="space-y-0.5">
              {group.items.map((section) => {
                const Icon = section.icon;
                return (
                  <button
                    key={section.id}
                    type="button"
                    data-active={active === section.id}
                    onClick={() => onSelect(section.id)}
                    className="glass-row nav-item gap-2.5 px-2.5 py-[7px]"
                  >
                    <Icon className="size-4 shrink-0" />
                    <span className="flex-1 truncate text-ui font-medium">
                      {section.label}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        ))}
      </nav>
    </GlassPanel>
  );
}

/**
 * 设置模式的右区：展示当前分区的内容。
 * 返回主页统一交给左栏顶部的「返回」入口，这里的头部只做面包屑标题，不再放返回按钮。
 */
export interface SettingsPanelProps {
  active: string;
}

export function SettingsPanel({ active }: SettingsPanelProps) {
  // 成员直达管理分区地址（书签/手输 URL，含裸 /settings 服务端兜底到的概览）
  // 时回退到首个可见分区——界面兜底，真正的安全边界在后端 403
  const { session } = useSession();
  const navigate = useNavigate();
  const allowed = settingsSectionGroupsFor(session.role).flatMap((g) => g.items);
  const section =
    allowed.find((s) => s.id === active) ?? allowed[0] ?? settingsSections[0];
  const Icon = section.icon;

  // 兜底展示的同时把地址替换成实际分区：URL 与内容一致、侧栏高亮不落空，
  // 刷新/分享当前页也不会再落回一个自己看不到的地址
  useEffect(() => {
    if (section.id !== active) {
      navigate(`/settings/${section.id}`, { replace: true });
    }
  }, [active, section.id, navigate]);

  return (
    // 无外框、无玻璃卡片：内容直接铺在全屏深色蒙版（.page-scrim）之上，
    // 背景透明，让蒙版透上来。沉浸式深色底，不再有圆角/描边/透出雪原的大卡片。
    // 头部与内容同列（同一 max-w 容器内），避免「标题贴左上、内容居中」的割裂感。
    <div className="scroll-thin scroll-safe h-full overflow-y-auto">
      {/* 分区按信息与操作密度自适应容器宽度：
          - 极高密度分区（日志、站点、下载器、全链路体检概览）：在平板及桌面宽屏从 max-w-4xl / 5xl 扩展至 max-w-7xl (1280px)，彻底解决大屏小气、信息挤压问题；
          - 表单与偏好类分区（外观、元数据、刮削设置、规则组、成员）：从 max-w-2xl 升级为 max-w-4xl / 2xl:max-w-5xl，宽屏下大方舒展，内部网格从 1 列变 2 列，滑块与字段不再拥挤；
          - 窄屏/移动端 (max-md)：维持 px-4 pb-12 pt-6，不受任何大屏扩展影响，WAP 移动端体验保持原汁原味 */}
      <div
        className={`mx-auto w-full px-6 pb-20 pt-12 max-md:px-4 max-md:pb-12 max-md:pt-6 ${
          section.id === "sites" ||
          section.id === "downloaders" ||
          section.id === "logs" ||
          section.id === "overview"
            ? "max-w-6xl xl:max-w-7xl"
            : section.id === "scrape" || section.id === "subscription" || section.id === "metadata" || section.id === "members"
            ? "max-w-4xl 2xl:max-w-5xl"
            : "max-w-3xl 2xl:max-w-4xl"
        }`}
      >
        {/* Netflix 移动端：分区名已由页顶的 NetflixSettingsNav（返回键 + 分区
            名）呈现，这里的大图标头在窄屏上重复占位（globals.css 按主题隐藏） */}
        <header className="settings-panel-head flex items-center gap-4">
          <span className="icon-chip size-12 !rounded-2xl">
            <Icon className="size-[22px]" />
          </span>
          <div className="min-w-0">
            {/* 实色 + 暗投影（不用 text-sheen 渐变裁切——全站蒙版默认轻档、
                背景大图透上来时，半透明渐变字压在亮背景上会发灰，实色白字最稳） */}
            <h1 className="text-on-image text-[22px] font-semibold tracking-tight text-[var(--text)] max-md:text-title-lg">
              {section.label}
            </h1>
            <p className="text-on-image mt-0.5 text-ui text-[var(--text-muted)]">
              {/* 资源站点分区的副标题是活的：有站点时显示健康统计，无站点回落介绍 */}
              {section.id === "sites" ? (
                <SitesSectionSubtitle fallback={section.description} />
              ) : (
                section.description
              )}
            </p>
          </div>
        </header>

        {/* 发丝分隔线：左亮右隐的渐变，呼应玻璃边缘的受光 */}
        <div className="settings-panel-head mb-8 mt-7 h-px bg-gradient-to-r from-white/[0.14] via-white/[0.06] to-transparent" />

        {section.id === "overview" ? (
          <SettingsOverviewSection />
        ) : section.id === "profile" ? (
          <ProfileSection />
        ) : section.id === "appearance" ? (
          <AppearanceSection />
        ) : section.id === "subscription" ? (
          <SubscriptionSettingsSection />
        ) : section.id === "metadata" ? (
          <MetadataSettingsSection />
        ) : section.id === "scrape" ? (
          <ScrapeSettingsSection />
        ) : section.id === "sites" ? (
          <SiteConfigSection />
        ) : section.id === "downloaders" ? (
          <DownloaderConfigSection />
        ) : section.id === "members" ? (
          <MembersSection />
        ) : section.id === "logs" ? (
          <SystemLogsSection />
        ) : null}
      </div>
    </div>
  );
}

/** 个人界面外观：主题值保存在用户偏好中，并即时更新全站外壳与颜色 token。 */
function AppearanceSection() {
  const { prefs, loading, savePrefs } = useUiPrefs();
  const toast = useToast();

  const selectTheme = async (themeId: string) => {
    try {
      await savePrefs({ ...prefs, theme: themeId });
    } catch (error) {
      toast.error((error as Error).message || "主题保存失败");
    }
  };

  return (
    <div className="space-y-8">
      <SettingsGroup label="界面主题">
        <p className="mb-3 text-sub leading-6 text-[var(--text-muted)]">
          主题会保存到你的账号，并在此设备和其他设备间同步。
        </p>
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {THEMES.map((theme) => {
            const selected = prefs.theme === theme.id;
            return (
              <button
                key={theme.id}
                type="button"
                aria-pressed={selected}
                disabled={loading}
                onClick={() => void selectTheme(theme.id)}
                className={`css-glass flex min-h-28 items-center gap-4 p-4 text-left transition disabled:cursor-wait disabled:opacity-60 ${
                  selected ? "border-[var(--accent)]/55 bg-[var(--accent-soft)]" : "hover:bg-[var(--glass-fill-hover)]"
                }`}
              >
                <span
                  aria-hidden="true"
                  className="grid size-12 shrink-0 place-items-center rounded-xl border border-[var(--line)] p-1.5"
                  style={{ background: theme.preview.bg }}
                >
                  <span
                    className="size-6 rounded-full shadow-sm"
                    style={{ background: theme.preview.accent }}
                  />
                </span>
                <span className="min-w-0">
                  <span className="block text-body font-semibold text-[var(--text)]">
                    {theme.label}
                    {selected && <span className="ml-2 text-caption text-[var(--accent)]">当前</span>}
                  </span>
                  <span className="mt-1 block text-caption leading-5 text-[var(--text-muted)]">
                    {theme.description}
                  </span>
                </span>
              </button>
            );
          })}
        </div>
      </SettingsGroup>
    </div>
  );
}

/**
 * 分组容器：小号大写分组标签 + 一张玻璃卡片。
 * 卡片内的行由使用方提供，多行时配合 divide-y 呈现 macOS 设置式的字段组。
 */
function SettingsGroup({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <section>
      <h3 className="group-label mb-2.5 px-1">{label}</h3>
      {children}
    </section>
  );
}

/* —— 个人信息分区（真实账号数据，来自登录会话）。
      新契约不再提供头像上传 / 改密 / 改昵称接口，本分区只读展示。 —— */
function ProfileSection() {
  const { session } = useSession();

  return (
    <div className="space-y-8">
      {/* 账号总览卡：头像 + 昵称 / 用户名 / 身份徽章 */}
      <div className="css-glass flex items-center gap-5 !rounded-2xl p-6">
        <AvatarBadge
          nickname={session.nickname}
          avatarUrl={session.avatar_url}
          className="size-[72px] text-2xl"
        />
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1">
            <p className="text-xl font-semibold tracking-tight">{session.nickname}</p>
            <span className="rounded-full border border-white/[0.12] bg-[var(--accent-soft)] px-2.5 py-0.5 text-caption font-semibold text-[var(--accent)]">
              {session.role === "member" ? "成员" : "超级管理员"}
            </span>
          </div>
          <p className="mt-1 text-body text-[var(--text-muted)]">@{session.username}</p>
        </div>
      </div>

      {/* 字段组：合并进一张卡片，行间发丝分隔（macOS 设置式），不再是散落的孤立圆角块 */}
      <SettingsGroup label="账号信息">
        <div className="css-glass divide-y divide-white/[0.055] !rounded-2xl">
          <FieldRow label="昵称" value={session.nickname} />
          <FieldRow label="用户名" value={session.username} hint="登录凭证，不可修改" />
        </div>
      </SettingsGroup>

      <SettingsGroup label="观看历史">
        <WatchHistoryCard />
      </SettingsGroup>
    </div>
  );
}

/** 清空自己全部观看记录（docs/design/library-access.md 2.6）：只删当前登录身份自己的行。 */
function WatchHistoryCard() {
  const confirm = useConfirm();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const clearAll = async () => {
    const ok = await confirm({
      title: "清空全部观看记录？",
      description:
        "所有作品的续播进度、已看标记和播放次数都会清除，首页「最近观看」与播放器的「继续观看」随即清空，无法恢复。只影响你自己的记录；应用更新前的自动备份仍包含历史记录。",
      confirmLabel: "清空",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(true);
    try {
      const { message } = await clearPlaybackHistory("all");
      toast.success(message);
    } catch (e) {
      toast.error((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="css-glass flex items-center justify-between gap-4 !rounded-2xl px-5 py-4">
      <div>
        <p className="text-body font-medium text-[var(--text)]">清空全部观看记录</p>
        <p className="mt-0.5 text-caption text-[var(--text-faint)]">
          续播进度、已看标记与播放次数一并清除；单部作品或单个库的记录可在对应页面的 ⋯ 菜单里清
        </p>
      </div>
      <button
        type="button"
        onClick={() => void clearAll()}
        disabled={busy}
        className="btn-glass h-9 shrink-0 px-4 text-ui font-medium !text-[#ff9f9f] disabled:opacity-40"
      >
        {busy ? "清空中…" : "清空"}
      </button>
    </div>
  );
}

/** 只读字段行（可附加说明文字）。 */
function FieldRow({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="flex items-center justify-between gap-4 px-5 py-4 first:rounded-t-2xl last:rounded-b-2xl">
      <div>
        <p className="text-body font-medium text-[var(--text)]">{label}</p>
        {hint && <p className="mt-0.5 text-caption text-[var(--text-faint)]">{hint}</p>}
      </div>
      <span className="text-body text-[var(--text-muted)]">{value}</span>
    </div>
  );
}
