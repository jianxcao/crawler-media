"use client";

import { useCallback, useEffect, useRef } from "react";

import {
  resolveLiquidGlassSettings,
  type LiquidGlassSettings,
  type LiquidGlassVariant,
} from "@/vendor/liquid-glass";
import { LiquidGlassRenderer } from "@/vendor/liquid-glass/core/LiquidGlassRenderer";
import { useTheme } from "@/lib/ui-prefs";

/**
 * GlassPanel —— 边到边大面板专用的「真实 WebGL 液态玻璃」承载层。
 *
 * 与 vendor 里的 LiquidGlassCard 的区别（也是我们不直接用 Card 的原因）：
 *   1. 去掉拖拽、最小尺寸、24px 内边距与渲染外扩 —— 面板要严丝合缝铺满容器。
 *
 * 背景采样开关（sampleBackground）——着色器最终颜色是
 *   mix(neutralGlass, sampledBackground, u_sampleBackground)：
 *   - true：采样并折射背景大图（主区用，透出《星际穿越》）。代价是面板可见时逐帧重绘。
 *   - false：只渲染合成的「中性玻璃色」（neutralGlass ≈ 近黑 + 竖向微渐变），
 *            边缘高光/镜面仍会叠加。用于 sidebar：底色恒为黑、不受背景明暗影响，
 *            与右侧「背景区」形成清晰区分（对齐参考站的深色侧栏）。
 *
 * 折射原理：渲染器按面板在视口中的位置，对同一张背景图（backgroundImage）做 cover
 * 采样。页面可见背景（body::before）也是同一张图、同样 cover 铺满视口，二者自然对齐，
 * 于是玻璃看起来就是「盖在背景图上的一块真玻璃」，边缘折射、透出下方图像。
 *
 * 降级：WebGL 不可用时静默失败，容器下方仍有 body 的 CSS 背景兜底，不会白屏。
 */
export interface GlassPanelProps {
  /** 背景图 URL，必须与 body 可见背景同源同图，折射才对得齐 */
  backgroundImage: string;
  /** 玻璃预设，面板默认用 dark（深色玻璃，透出被压暗的背景） */
  variant?: LiquidGlassVariant;
  /** 在预设基础上覆盖的参数（如调 darkTint 控制透出多少背景） */
  settings?: Partial<LiquidGlassSettings>;
  /** 圆角半径（px）。默认 16，与海报卡（rounded-2xl）同步；边到边面板可用 0 */
  radius?: number;
  /**
   * 背景采样强度。true/1=完全透出并折射背景大图（主区）；false/0=恒黑玻璃底；
   * 0~1 之间按比例混合——侧栏的「透明度」设置就映射到这里。
   */
  sampleBackground?: boolean | number;
  /**
   * CSS 发丝内描边的不透明度（写入 --glass-hairline 变量）。缺省用样式表默认
   * 的 0.07；侧栏透明度调高时传入递减值淡出这条线（见 lib/glass.ts）。
   */
  hairlineAlpha?: number;
  /**
   * 玻璃周边 CSS 装饰（WebGL 兜底底色 + 面板外投影）的强度系数 0~1，写入
   * --glass-fallback 变量（见 globals.css 的 .panel--sidebar 与 .glass-panel）。
   * 这些装饰不随 canvas 的 alpha 走：玻璃用 opacity 整体淡出时，兜底色会顶
   * 上来挡住页面、外投影会留下面板形状的暗晕，须传同比例递减值让它们一起
   * 淡出；缺省 1（全强度）。
   */
  fallbackAlpha?: number;
  /** 作用于最外层容器（尺寸 / 定位，如 h-full） */
  className?: string;
  /** 作用于内容层（承载子元素的布局，如 flex h-full flex-col） */
  contentClassName?: string;
  children?: React.ReactNode;
}

export function GlassPanel({
  backgroundImage,
  variant = "dark",
  settings,
  radius = 16,
  sampleBackground = true,
  hairlineAlpha,
  fallbackAlpha,
  className = "",
  contentClassName = "",
  children,
}: GlassPanelProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rendererRef = useRef<LiquidGlassRenderer | null>(null);
  const sizeRef = useRef({ width: 0, height: 0 });

  // Netflix（纯色平铺）与纸感玻璃（日光白瓷磨砂）停用深色 WebGL 采样，省下 GPU 与
  // 移动端 WebGL 上下文配额；银玻璃与液态玻璃共用同一 WebGL 生命周期，互切只热更新参数。
  const theme = useTheme();
  const usesWebgl = theme.id !== "netflix" && theme.id !== "paper";

  // 最新配置的引用：apply 从这里取值而非闭包捕获，保证参数热更新时
  // （如设置页拖动侧栏透明度滑杆、银玻璃 ↔ 液态玻璃互切）不必销毁重建 WebGL 渲染器。
  const propsRef = useRef({ variant, settings, radius, sampleBackground });
  propsRef.current = { variant, settings, radius, sampleBackground };
  const backgroundImageRef = useRef(backgroundImage);
  backgroundImageRef.current = backgroundImage;

  // 用序列化后的配置做依赖：内联传入的 settings 每次渲染都是新对象，
  // 直接进依赖会导致 effect 被反复触发，这里比对内容而非引用。
  const settingsKey = JSON.stringify({ variant, settings, radius, sampleBackground });

  /** 按当前容器尺寸与最新配置刷新渲染参数：玻璃透镜铺满整块面板。 */
  const apply = useCallback(() => {
    const renderer = rendererRef.current;
    const { width, height } = sizeRef.current;
    if (!renderer || !width || !height) return;
    const { variant, settings, radius, sampleBackground } = propsRef.current;
    // 1=折射透出背景大图；0=恒黑中性玻璃底；0~1 按比例混合（见组件顶部说明）。
    renderer.setBackgroundSampling(sampleBackground);
    renderer.resize(width, height);
    renderer.setSettings({
      ...resolveLiquidGlassSettings(variant, { radius, ...settings }),
      lensWidth: Math.max(1, width),
      lensHeight: Math.max(1, height),
      radius: Math.min(radius, height / 2),
    });
    renderer.setGeometry(width / 2, height / 2, 0, false, 1, 1, 0);
  }, []);

  // 渲染器仅随 WebGL 启用态（usesWebgl）创建/销毁；背景图切换走下方 setImage 热换纹理，
  // silver <-> liquid 互切直接经 apply() 更新 uniform，全程零 loseContext 闪烁与着色器重编译。
  useEffect(() => {
    if (!usesWebgl) return;
    const root = rootRef.current;
    const canvas = canvasRef.current;
    if (!root || !canvas) return;

    const { variant, settings, radius } = propsRef.current;
    let renderer: LiquidGlassRenderer;
    try {
      renderer = new LiquidGlassRenderer(
        canvas,
        backgroundImageRef.current,
        resolveLiquidGlassSettings(variant, { radius, ...settings }),
      );
    } catch (err) {
      console.warn("液态玻璃面板初始化失败，已降级为 CSS 背景：", err);
      return;
    }
    rendererRef.current = renderer;

    const observer = new ResizeObserver(([entry]) => {
      sizeRef.current = {
        width: entry.contentRect.width,
        height: entry.contentRect.height,
      };
      apply();
    });
    observer.observe(root);
    sizeRef.current = { width: root.clientWidth, height: root.clientHeight };
    apply();

    return () => {
      observer.disconnect();
      renderer.dispose();
      rendererRef.current = null;
    };
  }, [apply, usesWebgl]);

  // 背景图切换时原地更新 WebGL 纹理，不销毁重建上下文。
  useEffect(() => {
    rendererRef.current?.setImage(backgroundImage);
  }, [backgroundImage]);

  // 参数（variant/settings/radius/sampleBackground）内容变化时热更新，不重建渲染器。
  useEffect(() => {
    apply();
    // settingsKey 覆盖了上述参数的内容变化
  }, [settingsKey, apply]);

  const panelClass =
    theme.id === "netflix"
      ? "glass-panel glass-panel--flat"
      : theme.id === "paper"
        ? "glass-panel glass-panel--paper"
        : "glass-panel";

  return (
    // 四套主题共用同一外层 <div> 与固定子槽位（slot 0 = canvas|null，slot 1 = 内容层）：
    // 任意主题切换都不会卸载/重建 {children}。
    <div
      ref={rootRef}
      className={`${panelClass} ${className}`}
      style={
        {
          // Netflix 全站圆角压到 2/4/8 档，内联 radius 在此钳到 8px；其余主题保持原值
          borderRadius: theme.id === "netflix" ? Math.min(radius, 8) : radius,
          ...(theme.id !== "netflix" &&
            hairlineAlpha !== undefined && { "--glass-hairline": hairlineAlpha }),
          ...(theme.id !== "netflix" &&
            fallbackAlpha !== undefined && { "--glass-fallback": fallbackAlpha }),
        } as React.CSSProperties
      }
    >
      {usesWebgl ? <canvas ref={canvasRef} className="glass-panel__canvas" aria-hidden="true" /> : null}
      <div className={`glass-panel__content ${contentClassName}`}>{children}</div>
    </div>
  );
}
