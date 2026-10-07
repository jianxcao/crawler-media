import type { LibraryKind } from "@/lib/api/libraries";

/**
 * 媒体库卡片默认矢量封面（21:10 比例）。
 * 针对电影库（Movie）、剧集库（TV）与自制视频（Video）定制高清晰度现代流媒体视觉：
 * 融合柔和放射光晕、精致胶片与放映机光锥、剧集多季画卷层次，
 * 在任何屏幕（Retina / 4K）上都保持极度锐利与深邃质感。
 */
export function LibraryKindCoverArt({
  kind,
  className = "size-full",
}: {
  kind: LibraryKind;
  className?: string;
}) {
  if (kind === "tv") {
    return <TvCoverArt className={className} />;
  }
  if (kind === "video") {
    return <VideoCoverArt className={className} />;
  }
  return <MovieCoverArt className={className} />;
}

/** 电影库默认封面：午夜深蓝底色 + 放映光锥 + 胶卷盘与场记板透镜微光 */
function MovieCoverArt({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 420 200"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      preserveAspectRatio="xMidYMid slice"
    >
      <defs>
        <linearGradient id="movie-bg" x1="0" y1="0" x2="420" y2="200" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#0B0F19" />
          <stop offset="50%" stopColor="#111728" />
          <stop offset="100%" stopColor="#070A11" />
        </linearGradient>

        <radialGradient id="movie-radial" cx="210" cy="100" r="140" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#38BDF8" stopOpacity="0.22" />
          <stop offset="45%" stopColor="#6366F1" stopOpacity="0.08" />
          <stop offset="100%" stopColor="#000000" stopOpacity="0" />
        </radialGradient>

        <linearGradient id="movie-film-grad" x1="170" y1="50" x2="250" y2="150" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#1E293B" />
          <stop offset="100%" stopColor="#0F172A" />
        </linearGradient>

        <linearGradient id="movie-accent" x1="150" y1="60" x2="270" y2="140" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#38BDF8" />
          <stop offset="100%" stopColor="#818CF8" />
        </linearGradient>
      </defs>

      <rect width="420" height="200" fill="url(#movie-bg)" />
      <circle cx="210" cy="100" r="140" fill="url(#movie-radial)" />

      <path d="M210 20 L270 180 L150 180 Z" fill="url(#movie-radial)" opacity="0.6" />

      <g opacity="0.08" stroke="#FFFFFF" strokeWidth="1">
        <line x1="40" y1="100" x2="380" y2="100" strokeDasharray="4 6" />
        <line x1="210" y1="20" x2="210" y2="180" strokeDasharray="4 6" />
        <circle cx="210" cy="100" r="75" fill="none" strokeDasharray="2 4" />
      </g>

      <g transform="translate(160, 48)">
        <circle cx="50" cy="52" r="46" fill="url(#movie-film-grad)" stroke="rgba(255,255,255,0.12)" strokeWidth="1.5" />
        <circle cx="50" cy="52" r="38" stroke="rgba(56,189,248,0.25)" strokeWidth="1" strokeDasharray="3 3" />

        <circle cx="50" cy="26" r="8" fill="#080B12" stroke="rgba(255,255,255,0.1)" strokeWidth="1" />
        <circle cx="50" cy="78" r="8" fill="#080B12" stroke="rgba(255,255,255,0.1)" strokeWidth="1" />
        <circle cx="24" cy="52" r="8" fill="#080B12" stroke="rgba(255,255,255,0.1)" strokeWidth="1" />
        <circle cx="76" cy="52" r="8" fill="#080B12" stroke="rgba(255,255,255,0.1)" strokeWidth="1" />

        <circle cx="50" cy="52" r="13" fill="#0B0F19" stroke="url(#movie-accent)" strokeWidth="2" />
        <circle cx="50" cy="52" r="4.5" fill="#38BDF8" />

        <g transform="rotate(-12 50 52)">
          <rect x="22" y="4" width="62" height="15" rx="3.5" fill="#0F172A" stroke="rgba(255,255,255,0.2)" strokeWidth="1" />
          <path d="M30 4 L37 19 M44 4 L51 19 M58 4 L65 19 M72 4 L79 19" stroke="url(#movie-accent)" strokeWidth="3" strokeLinecap="round" opacity="0.85" />
        </g>
      </g>

      <text
        x="210"
        y="166"
        textAnchor="middle"
        fill="rgba(255,255,255,0.38)"
        fontSize="11"
        fontWeight="600"
        letterSpacing="3.5"
        style={{ fontFamily: "system-ui, -apple-system, sans-serif" }}
      >
        FEATURE FILMS
      </text>
    </svg>
  );
}

/** 剧集库默认封面：深邃紫罗兰底色 + 多季连续剧画卷层叠 + 广播天线光带 */
function TvCoverArt({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 420 200"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      preserveAspectRatio="xMidYMid slice"
    >
      <defs>
        <linearGradient id="tv-bg" x1="0" y1="0" x2="420" y2="200" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#0F0C1B" />
          <stop offset="50%" stopColor="#1A122E" />
          <stop offset="100%" stopColor="#080611" />
        </linearGradient>

        <radialGradient id="tv-radial" cx="210" cy="100" r="140" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#EC4899" stopOpacity="0.2" />
          <stop offset="45%" stopColor="#8B5CF6" stopOpacity="0.08" />
          <stop offset="100%" stopColor="#000000" stopOpacity="0" />
        </radialGradient>

        <linearGradient id="tv-screen-grad" x1="160" y1="40" x2="260" y2="150" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#2E1B4E" />
          <stop offset="100%" stopColor="#160E29" />
        </linearGradient>

        <linearGradient id="tv-accent" x1="170" y1="50" x2="250" y2="140" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#F43F5E" />
          <stop offset="100%" stopColor="#A855F7" />
        </linearGradient>
      </defs>

      <rect width="420" height="200" fill="url(#tv-bg)" />
      <circle cx="210" cy="100" r="140" fill="url(#tv-radial)" />

      <g opacity="0.1" stroke="#FFFFFF" strokeWidth="1">
        <circle cx="210" cy="96" r="70" fill="none" strokeDasharray="3 4" />
        <circle cx="210" cy="96" r="105" fill="none" strokeDasharray="4 6" />
        <line x1="50" y1="96" x2="370" y2="96" strokeDasharray="3 6" />
      </g>

      <g transform="translate(155, 42)">
        <rect
          x="30"
          y="12"
          width="74"
          height="52"
          rx="8"
          fill="#1C1433"
          stroke="rgba(255,255,255,0.06)"
          strokeWidth="1"
          transform="rotate(6 67 38)"
        />

        <rect
          x="14"
          y="18"
          width="78"
          height="55"
          rx="9"
          fill="url(#tv-screen-grad)"
          stroke="rgba(255,255,255,0.12)"
          strokeWidth="1.2"
        />

        <g transform="translate(14, 18)">
          <rect x="5" y="5" width="68" height="45" rx="5" fill="#0E081D" />
          <path d="M12 28 L24 28 L30 18 L38 36 L44 24 L50 28 L60 28" stroke="url(#tv-accent)" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" opacity="0.85" />
          <circle cx="38" cy="36" r="2.5" fill="#F43F5E" />
        </g>

        <path d="M43 73 L63 73 M53 68 L53 73" stroke="rgba(255,255,255,0.25)" strokeWidth="2" strokeLinecap="round" />

        <path d="M40 9 L53 18 L66 9" stroke="url(#tv-accent)" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" opacity="0.9" />
        <circle cx="53" cy="18" r="2" fill="#FFFFFF" />
      </g>

      <text
        x="210"
        y="166"
        textAnchor="middle"
        fill="rgba(255,255,255,0.38)"
        fontSize="11"
        fontWeight="600"
        letterSpacing="3.5"
        style={{ fontFamily: "system-ui, -apple-system, sans-serif" }}
      >
        TV & SERIES
      </text>
    </svg>
  );
}

/** 其他视频/家庭影像封面：深邃墨黑 + 取景十字准星 + 胶卷框 */
function VideoCoverArt({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 420 200"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      preserveAspectRatio="xMidYMid slice"
    >
      <defs>
        <linearGradient id="vid-bg" x1="0" y1="0" x2="420" y2="200" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#0B1215" />
          <stop offset="50%" stopColor="#102026" />
          <stop offset="100%" stopColor="#060A0C" />
        </linearGradient>
        <radialGradient id="vid-radial" cx="210" cy="100" r="140" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#10B981" stopOpacity="0.18" />
          <stop offset="100%" stopColor="#000000" stopOpacity="0" />
        </radialGradient>
      </defs>
      <rect width="420" height="200" fill="url(#vid-bg)" />
      <circle cx="210" cy="100" r="140" fill="url(#vid-radial)" />
      <g transform="translate(170, 55)" stroke="#10B981" strokeWidth="2" strokeLinecap="round">
        <path d="M10 25 L10 10 L25 10 M70 10 L85 10 L85 25 M85 55 L85 70 L70 70 M25 70 L10 70 L10 55" opacity="0.6" />
        <circle cx="48" cy="40" r="14" fill="none" strokeWidth="1.5" opacity="0.8" />
        <circle cx="48" cy="40" r="3" fill="#10B981" />
      </g>
      <text
        x="210"
        y="166"
        textAnchor="middle"
        fill="rgba(255,255,255,0.38)"
        fontSize="11"
        fontWeight="600"
        letterSpacing="3.5"
        style={{ fontFamily: "system-ui, -apple-system, sans-serif" }}
      >
        VIDEO COLLECTION
      </text>
    </svg>
  );
}
