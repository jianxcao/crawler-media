import type { UiPreferences } from "@/lib/api/ui";

/**
 * 界面偏好的首帧缓存（localStorage）。
 *
 * 为什么需要：`ui.preferences` 是启动后异步拉回来的（见 lib/ui-prefs.tsx），
 * 在它返回之前全站只能按内置默认渲染——自定义过导航顺序的用户每次刷新都会
 * 先看到默认排序、接口回来后再重排一次，肉眼可见地"跳"一下（玻璃透明度、
 * 蒙版同理）。这里把上一次拿到的偏好整份存下来，下次启动时首帧就直接按它
 * 渲染，接口返回后再以服务端值为准纠正。
 *
 * 与 lib/backdrop-cache.ts 是同一套思路，同样刻意不 import 任何运行时模块
 * （只用 `import type`）：http.ts 在 401 时要清它，带运行时依赖会形成循环。
 *
 * 三条约定：
 *   - **缓存只是首帧优化**，真实状态永远以 GET /ui/preferences 为准；缓存脏了
 *     （换了设备上改的顺序）也只是首帧短暂显示旧值，接口回来即纠正；
 *   - 读出来的内容不可信（用户可改、也可能被写坏）：不是对象字面量就当没有，
 *     由调用方退回内置默认；
 *   - **账号切换时必须清除**，否则共享浏览器上换个账号登录会先闪出上一个人的
 *     导航顺序（清除点与背景缓存一致：登录成功、退出登录、401 跳登录页）。
 */
export const UI_PREFS_CACHE_KEY = "crawler-media.ui-prefs";

const DEFAULT_CACHED_UI_PREFS: UiPreferences = {
  theme: "silver",
  sidebar: { transparency: 0.49, brightness: -0.36, depth: 28 },
  scrim: { blur: 13, dark: 0.69 },
  nav: { order: [] },
  home: { rows: [] },
};

/**
 * 补齐偏好对象中缺失的子字段（防御空对象 `{}`、旧版本缓存或损坏属性）。
 */
export function normalizeUiPrefs(parsed: unknown): UiPreferences {
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return DEFAULT_CACHED_UI_PREFS;
  }
  const p = parsed as Partial<UiPreferences>;
  return {
    theme: typeof p.theme === "string" ? p.theme : DEFAULT_CACHED_UI_PREFS.theme,
    sidebar: {
      transparency:
        typeof p.sidebar?.transparency === "number"
          ? p.sidebar.transparency
          : DEFAULT_CACHED_UI_PREFS.sidebar.transparency,
      brightness:
        typeof p.sidebar?.brightness === "number"
          ? p.sidebar.brightness
          : DEFAULT_CACHED_UI_PREFS.sidebar.brightness,
      depth:
        typeof p.sidebar?.depth === "number"
          ? p.sidebar.depth
          : DEFAULT_CACHED_UI_PREFS.sidebar.depth,
    },
    scrim: {
      blur:
        typeof p.scrim?.blur === "number"
          ? p.scrim.blur
          : DEFAULT_CACHED_UI_PREFS.scrim.blur,
      dark:
        typeof p.scrim?.dark === "number"
          ? p.scrim.dark
          : DEFAULT_CACHED_UI_PREFS.scrim.dark,
    },
    nav: {
      order: Array.isArray(p.nav?.order) ? p.nav.order : DEFAULT_CACHED_UI_PREFS.nav.order,
    },
    home: {
      rows: Array.isArray(p.home?.rows) ? p.home.rows : DEFAULT_CACHED_UI_PREFS.home.rows,
    },
  };
}

/** 读取上次缓存的偏好；无缓存 / 已损坏 / localStorage 不可用时返回 null
 *  （调用方退回内置默认）。 */
export function readUiPrefsCache(): UiPreferences | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = localStorage.getItem(UI_PREFS_CACHE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    // 只接受对象字面量：数组、字符串、null 都是被写坏的缓存，直接当没有
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return normalizeUiPrefs(parsed);
  } catch {
    return null;
  }
}

/** 写入首帧缓存。失败（隐私模式、配额满）只是失去首帧优化，不影响功能。 */
export function writeUiPrefsCache(prefs: UiPreferences): void {
  if (typeof window === "undefined") return;
  try {
    localStorage.setItem(UI_PREFS_CACHE_KEY, JSON.stringify(prefs));
  } catch {
    // 忽略写入失败
  }
}

/** 清除首帧缓存。账号切换时必须调用，避免共享浏览器短暂显示上一账号的界面偏好。 */
export function clearUiPrefsCache(): void {
  if (typeof window === "undefined") return;
  try {
    localStorage.removeItem(UI_PREFS_CACHE_KEY);
  } catch {
    // localStorage 不可用只会失去首帧优化，不影响服务端的账号隔离。
  }
}
