/**
 * Netflix 主题的品牌标识（字标 + 字母标）。
 *
 * - 字标：crawler-media，重型压缩无衬线（Impact / Arial Black 同族）——用
 *   `<text>` + 系统字体栈实现，不内联字形路径，随容器缩放不失真；色值固化为
 *   品牌红 #e50914（Netflix 品牌色不随主题 token 变化，token 只管界面皮肤）。
 * - 字母标：胶片条 + 三指爪（与 `public/logo-mark.svg` / `scripts/gen-brand.py`
 *   同一套几何，viewBox 0 0 160 160）：奶油色胶片条带片孔，橙色爪指从掌根
 *   扇形张开钩住胶片——「爬爪拉片」的意象，暗底上直接可用（无底、透明）。
 *
 * 两个组件都是纯内联 SVG：无网络请求、随容器缩放不失真。
 */
const WORDMARK_FONT = [
  "Impact",
  '"Arial Black"',
  '"Helvetica Neue"',
  "Arial",
  "system-ui",
  "sans-serif",
].join(", ");

/** crawler-media 字标：红色重型无衬线，viewBox 按字形比例留足边距。 */
export function CrawlerWordmark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 612 88" className={className} role="img" aria-label="crawler-media">
      <text
        x="2"
        y="80"
        fontFamily={WORDMARK_FONT}
        fontWeight="900"
        fontSize="100"
        letterSpacing="-1.5"
        fill="#e50914"
      >
        crawler-media
      </text>
    </svg>
  );
}

/**
 * 字母标：胶片条 + 三指爪（viewBox 0 0 160 160，与 logo-mark.svg 同几何）。
 *
 * 层序：胶片条（奶油 #F4EEE2，四片孔回填底色）→ 爪指（亮橙 #FF6A3D 描边，
 * 二次贝塞尔转三次、圆头）。爪指从同一掌根（26,80）扇形张开，尖端压住胶片
 * 条左缘——「爬爪拉片」。透明底，适配暗色主题直接叠放。
 */
export function CrawlerMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 160 160" className={className} role="img" aria-label="crawler-media">
      <defs>
        <linearGradient id="cm-bg" x1="16" y1="12" x2="144" y2="148" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#181B26"/>
          <stop offset="100%" stopColor="#08090E"/>
        </linearGradient>
        <linearGradient id="cm-play-grad" x1="56" y1="36" x2="134" y2="108" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#6366F1"/>
          <stop offset="48%" stopColor="#EC4899"/>
          <stop offset="100%" stopColor="#FF5722"/>
        </linearGradient>
        <linearGradient id="cm-film" x1="26" y1="28" x2="74" y2="132" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#242838"/>
          <stop offset="100%" stopColor="#12141D"/>
        </linearGradient>
      </defs>
      <rect x="8" y="8" width="144" height="144" rx="36" fill="url(#cm-bg)"/>
      <rect x="8.5" y="8.5" width="143" height="143" rx="35.5" stroke="rgba(255,255,255,0.15)" strokeWidth="1"/>
      <rect x="28" y="32" width="40" height="96" rx="9" fill="url(#cm-film)"/>
      <rect x="34" y="42" width="9" height="14" rx="3" fill="#08090E"/>
      <rect x="34" y="65" width="9" height="14" rx="3" fill="#08090E"/>
      <rect x="34" y="88" width="9" height="14" rx="3" fill="#08090E"/>
      <rect x="34" y="104" width="9" height="14" rx="3" fill="#08090E"/>
      <path d="M60 41.5 C60 38.2 63.8 36.3 66.5 38.1 L124.8 76.6 C127.2 78.2 127.2 81.8 124.8 83.4 L66.5 121.9 C63.8 123.7 60 121.8 60 118.5 Z" fill="url(#cm-play-grad)"/>
      <path d="M72 58 C79 66 84 74 84 80 C84 86 79 94 72 102" stroke="rgba(255,255,255,0.45)" strokeWidth="4.5" strokeLinecap="round" fill="none"/>
      <circle cx="102" cy="80" r="4.5" fill="#FFFFFF" opacity="0.9"/>
    </svg>
  );
}
