/**
 * 液态玻璃组件的共享入口。
 *
 * vendor/liquid-glass 是从 liquidglass-oss 项目按 MIT 许可内联进来的 WebGL 组件源码
 * （见 vendor/liquid-glass/LICENSE）。这里统一导出常量与类型，方便各处引用。
 *
 * BACKDROP：所有玻璃组件都要传入的背景图 URL。着色器会按元素在视口中的位置
 * 采样这张图做折射，因此它必须与 body 的固定背景是同一张、且同源（放在 public/ 下）。
 */
export const BACKDROP = "/backdrop-default.jpg";

/**
 * 侧栏玻璃 = LiquidGlassCard 的同款材质（用户明确要求「完全还原 Card 质感」）。
 * 这组覆盖逐字段抄自 vendor/LiquidGlassCard.tsx 的 mergedSettings，叠在它的
 * 默认 frosted 变体之上；配合「始终采样背景」（Card 恒为 setBackgroundSampling(true)），
 * 侧栏呈现与 Card 一致的「透光磨砂玻璃 + 微弱材质羽化（opacity .96）」。
 * tint / depth / opacity 三项是设置页滑杆的调节对象，由 sidebarGlass() 注入，
 * 不在此写死。工作台与设置页侧栏共用，改一处即可。
 */
export const SIDEBAR_GLASS = {
  blur: 0.34,
  refraction: 0.38,
  chromaticAberration: 0.025,
  distortion: 0.012,
  edgeHighlight: 0.1,
  specular: 0.12,
  fresnel: 0.9,
  darkTint: 0.12,
  tintStrength: 0.1,
  shadow: 0.18,
  bevel: 0,
} as const;

/** 液态玻璃主题侧栏：启用曲面透镜（bevel=1）+ 更高光学折射率、色散与高光反射。 */
export const LIQUID_SIDEBAR_GLASS = {
  blur: 0.16,
  refraction: 0.86,
  chromaticAberration: 0.11,
  distortion: 0.024,
  edgeHighlight: 0.22,
  specular: 0.26,
  fresnel: 1.18,
  darkTint: 0.06,
  tintStrength: 0.16,
  tintColor: [0.82, 0.95, 1.08] as [number, number, number],
  shadow: 0.2,
  bevel: 1,
  liquidMotion: 0.18,
} as const;

/** 侧栏玻璃的变体：银玻璃用 frosted 磨砂，液态玻璃用 dome 曲面透镜。 */
export const SIDEBAR_GLASS_VARIANT = "frosted" as const;
export const LIQUID_SIDEBAR_GLASS_VARIANT = "dome" as const;

/**
 * 由用户偏好与当前主题推导侧栏 GlassPanel 的入参（见 lib/api/ui.ts 的 SidebarUiPrefs）。
 * 银玻璃基底是 Card 同款磨砂材质；液态玻璃升级为曲面光学折射（dome + bevel=1 + 色散高光）。
 */
export function sidebarGlass(
  prefs: {
    transparency: number;
    brightness: number;
    depth: number;
  },
  themeId?: string,
) {
  if (themeId === "liquid") {
    return {
      variant: LIQUID_SIDEBAR_GLASS_VARIANT,
      sampleBackground: 1,
      settings: {
        ...LIQUID_SIDEBAR_GLASS,
        tint: prefs.brightness,
        depth: Math.max(44, prefs.depth),
        opacity: 0.94 * (1 - prefs.transparency),
      },
      hairlineAlpha: 0.18 * (1 - prefs.transparency),
      fallbackAlpha: 1 - prefs.transparency,
    };
  }
  return {
    variant: SIDEBAR_GLASS_VARIANT,
    sampleBackground: 1,
    settings: {
      ...SIDEBAR_GLASS,
      tint: prefs.brightness,
      depth: prefs.depth,
      opacity: 0.96 * (1 - prefs.transparency),
    },
    hairlineAlpha: 0,
    fallbackAlpha: 1 - prefs.transparency,
  };
}
