#!/usr/bin/env python3
"""crawler-media 品牌资产生成脚本（PIL，无第三方依赖）。

产出：
- public/logo-mark.svg             —— favicon 用的 mark（由 mark_svg() 手写同步）
- public/crawler-logo.png          —— 横版 lockup：mark + 字标（透明底，1920x525）
- public/crawler-logo-mark.png     —— 独立 mark（透明底，525x525）
- public/icons/icon-192/512.png    —— PWA 图标（深底圆角方 + mark，含 maskable 安全区）
- public/apple-touch-icon.png      —— iOS 图标（180x180，深底圆角方 + mark）
- public/splash/*.png              —— iOS 启动屏（#0a0b10 纯色底 + 居中字标）

依赖系统字体 Impact（与品牌「重型压缩无衬线」字标同族）。改 mark 几何时，
记得同步 public/logo-mark.svg 与 web/components/netflix/brand.tsx 的 CrawlerMark。
"""
import os
import math
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PUBLIC = os.path.join(ROOT, "public")

# 品牌色（与 logo-mark.svg / netflix 主题一致）
BG = (17, 17, 17)          # #111111
CREAM = (244, 238, 226)    # #F4EEE2
ORANGE = (255, 106, 61)    # #FF6A3D
SPLASH_BG = (10, 11, 16)   # #0a0b10

# mark 几何（viewBox 0 0 160 160，与 logo-mark.svg 同步）
STRIP = dict(x=97, y=30, w=35, h=100, r=8)          # 胶片条
HOLES = [(109, 44), (109, 63), (109, 82), (109, 101)]  # 片孔 (x, y)，10x14 rx3
CLAWS = [                                              # 爪指：Q 曲线 (sx,sy,cx,cy,ex,ey)
    # 三指从同一「掌根」出发，扇形张开钩住胶片条
    (26, 80, 76, 46, 104, 54),
    (26, 80, 86, 78, 106, 82),
    (26, 80, 76, 114, 104, 106),
]
CLAW_WIDTH = 13
CLAW_COLOR = ORANGE
STRIP_COLOR = CREAM

FONT_PATH = "/System/Library/Fonts/Supplemental/Impact.ttf"


def _font(px: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(FONT_PATH, px)


def _quad(p0, c, p1, n=28):
    """二次贝塞尔采样点列表（PIL 无真曲线，逐点折线近似）。"""
    pts = []
    for i in range(n + 1):
        t = i / n
        u = 1 - t
        pts.append((u * u * p0[0] + 2 * u * t * c[0] + t * t * p1[0],
                    u * u * p0[1] + 2 * u * t * c[1] + t * t * p1[1]))
    return pts


def mark_shapes(d: ImageDraw.ImageDraw, s: float = 1.0):
    """把 mark 画到 (160*s) 画布上（透明底）。s=缩放系数。"""
    def P(*xy):  # 按缩放系数映射坐标
        return tuple(v * s for v in xy)

    def R(x, y, w, h, r):
        d.rounded_rectangle(P(x, y, x + w, y + h), radius=r * s, fill=STRIP_COLOR)

    # 胶片条
    st = STRIP
    R(st["x"], st["y"], st["w"], st["h"], st["r"])
    # 片孔（镂空感：画回底色）
    for hx, hy in HOLES:
        d.rounded_rectangle(P(hx, hy, hx + 10, hy + 14), radius=3 * s, fill=BG)
    # 爪指（亮橙描边曲线，圆头）
    lw = int(CLAW_WIDTH * s)
    for sx, sy, cx, cy, ex, ey in CLAWS:
        pts = _quad((sx, sy), (cx, cy), (ex, ey))
        d.line([P(*p) for p in pts], fill=CLAW_COLOR, width=lw, joint="curve")
        for exx, eyy in ((sx, sy), (ex, ey)):  # 圆头：两端补圆
            d.ellipse(P(exx - CLAW_WIDTH / 2, eyy - CLAW_WIDTH / 2,
                        exx + CLAW_WIDTH / 2, eyy + CLAW_WIDTH / 2), fill=CLAW_COLOR)


def rounded_square(d: ImageDraw.ImageDraw, size: int, radius: int, fill=BG):
    d.rounded_rectangle((0, 0, size, size), radius=radius, fill=fill)


def render_mark(size: int, with_bg: bool, radius_ratio: float = 0.225) -> Image.Image:
    im = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    if with_bg:
        rounded_square(d, size, int(size * radius_ratio))
        # mark 内容占中间 78%（给 maskable 留安全区）
        inner = size * 0.78
        s = inner / 160.0
        ox = (size - inner) / 2.0
        # 用子画布绘制再合成，避免逐点偏移
        sub = Image.new("RGBA", (size, size), (0, 0, 0, 0))
        sd = ImageDraw.Draw(sub)
        mark_shapes(sd, s)
        im.alpha_composite(sub, (int(ox), int(ox)))
    else:
        mark_shapes(d, size / 160.0)
    return im


def render_lockup(width: int = 1920, height: int = 525) -> Image.Image:
    im = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    mark_s = height / 160.0 * 1.05          # mark 略放大（视觉对齐字标）
    mark_side = 160 * mark_s
    mx = int(height * 0.10)
    my = int((height - mark_side) / 2)
    sub = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    sd = ImageDraw.Draw(sub)
    mark_shapes(sd, mark_s)
    im.alpha_composite(sub, (mx, my))
    # 字标：Impact，垂直居中，左对齐 mark 右侧
    word = "crawler-media"
    fs = int(height * 0.52)
    f = _font(fs)
    bbox = d.textbbox((0, 0), word, font=f)
    tw = bbox[2] - bbox[0]
    tx = mx + int(mark_side) + int(height * 0.10)
    ty = int((height - (bbox[3] - bbox[1])) / 2) - bbox[1]
    d.text((tx, ty), word, font=f, fill=CREAM)
    return im


def render_splash(w: int, h: int) -> Image.Image:
    im = Image.new("RGBA", (w, h), SPLASH_BG + (255,))
    d = ImageDraw.Draw(im)
    word = "crawler-media"
    fs = int(h * 0.055)
    f = _font(fs)
    bbox = d.textbbox((0, 0), word, font=f)
    tw = bbox[2] - bbox[0]
    tx = (w - tw) // 2
    ty = (h - (bbox[3] - bbox[1])) // 2 - bbox[1]
    d.text((tx, ty), word, font=f, fill=CREAM)
    return im


def main():
    # lockup + mark
    render_lockup().save(os.path.join(PUBLIC, "crawler-logo.png"))
    render_mark(525, with_bg=False).save(os.path.join(PUBLIC, "crawler-logo-mark.png"))
    # PWA 图标 + apple-touch（深底圆角方，mark 居安全区）
    for s in (192, 512):
        render_mark(s, with_bg=True).save(os.path.join(PUBLIC, "icons", f"icon-{s}.png"))
    render_mark(180, with_bg=True, radius_ratio=0.225).save(
        os.path.join(PUBLIC, "apple-touch-icon.png")
    )
    # 启动屏
    splash_dir = os.path.join(PUBLIC, "splash")
    if os.path.isdir(splash_dir):
        for fn in sorted(os.listdir(splash_dir)):
            if not fn.endswith(".png"):
                continue
            parts = fn[len("splash-"):].split("@")
            w, h = (int(v) for v in parts[0].split("x"))
            r = int(parts[1][0])
            render_splash(w * r, h * r).save(os.path.join(splash_dir, fn))
    print("done")


if __name__ == "__main__":
    main()
