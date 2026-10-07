import { CrawlerMark } from "@/components/netflix/brand";

/**
 * 全站统一的品牌加载指示：字母标 + 呼吸动画（见 globals.css 的 .brand-loader）。
 *
 * 取代原页面/区块级加载位的圆形 border spinner（那些散落在二十多个文件里的
 * `animate-spin rounded-full border-2` 手写体）。按钮内联的微型 spinner
 * （border-current 小尺寸那种）刻意不换——字母标塞进按钮里太吵。
 *
 * 尺寸由调用方给（默认 20px，约等于原 size-4/size-5 spinner 的视觉体量；
 * 字母标细节多，比同尺寸圆环要放大一档才等观感）。动画尊重系统
 * prefers-reduced-motion（globals.css 里降级为静止）。
 *
 * 品牌随主题分叉（同一 DOM 双渲染 + CSS 显隐，组件保持无状态、可在服务端
 * 组件里使用）：银玻璃 = 图片 logo；Netflix = SVG 字母标
 * （netflix 主题下再出现玻璃 logo 是「半成品感」的主要来源之一）。
 */
export function BrandLoader({ className = "size-5" }: { className?: string }) {
  return (
    <span aria-hidden="true" className={`brand-loader inline-block shrink-0 ${className}`}>
      <img
        src="/logo-mark.svg"
        alt=""
        draggable={false}
        className="brand-loader--silver size-full object-contain"
      />
      <CrawlerMark className="brand-loader--netflix size-full" />
    </span>
  );
}
