"use client";

import { useState } from "react";
import { Modal } from "@/components/modal";
import { publicEnv } from "@/lib/env";
import {
  generateLibraryCover,
  uploadLibraryCover,
  deleteLibraryCover,
  MediaLibrary,
} from "@/lib/api/libraries";

interface LibraryCoverModalProps {
  library: MediaLibrary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCoverUpdated?: () => void;
}

export function LibraryCoverModal({
  library,
  open,
  onOpenChange,
  onCoverUpdated,
}: LibraryCoverModalProps) {
  const currentCoverSrc = `${publicEnv.apiBaseUrl}/libraries/${library.id}/cover?t=${Date.now()}`;

  const [activeTab, setActiveTab] = useState<"generate" | "upload">("generate");
  const [previewSrc, setPreviewSrc] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 生成参数
  const [titleZh, setTitleZh] = useState(library.name);
  const [titleEn, setTitleEn] = useState("");
  const [style, setStyle] = useState<"macaron_card_single" | "multi_poster_pile">(
    "macaron_card_single",
  );
  const [bgMode, setBgMode] = useState<"auto" | "gradient_dark" | "solid_dark">(
    "auto",
  );

  // 1. 生成预览
  const handleGenerate = async (previewOnly: boolean) => {
    // 如果已经预览过了，且点击「保存设为封面」，直接复用当前的 previewSrc 落盘，零等待！
    if (!previewOnly && previewSrc) {
      setLoading(true);
      setError(null);
      try {
        await uploadLibraryCover(library.id, previewSrc);
        onCoverUpdated?.();
        onOpenChange(false);
      } catch (e: any) {
        setError(e.message || "保存封面失败");
      } finally {
        setLoading(false);
      }
      return;
    }

    setLoading(true);
    setError(null);
    try {
      let bgPayload: any = { mode: "auto_extract", blur_radius: 50, color_ratio: 0.8 };
      if (bgMode === "gradient_dark") {
        bgPayload = { mode: "gradient", from_hex: "#1f2438", to_hex: "#0c0d14", angle: 0 };
      } else if (bgMode === "solid_dark") {
        bgPayload = { mode: "solid_color", hex_color: "#121824" };
      }

      const res = await generateLibraryCover(library.id, {
        title_zh: titleZh || library.name,
        title_en: titleEn || undefined,
        style,
        background: bgPayload,
        preview_only: previewOnly,
      });

      setPreviewSrc(res.data_url);
      if (!previewOnly) {
        onCoverUpdated?.();
        onOpenChange(false);
      }
    } catch (e: any) {
      setError(e.message || "生成封面失败");
    } finally {
      setLoading(false);
    }
  };

  // 2. 本地文件上传
  const handleFileUpload = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;

    const reader = new FileReader();
    reader.onload = async () => {
      const dataUrl = reader.result as string;
      setLoading(true);
      setError(null);
      try {
        await uploadLibraryCover(library.id, dataUrl);
        onCoverUpdated?.();
        onOpenChange(false);
      } catch (err: any) {
        setError(err.message || "上传失败");
      } finally {
        setLoading(false);
      }
    };
    reader.readAsDataURL(file);
  };

  // 3. 恢复默认（清空）
  const handleDeleteCover = async () => {
    if (!confirm("确定要恢复默认封面吗？")) return;
    setLoading(true);
    setError(null);
    try {
      await deleteLibraryCover(library.id);
      onCoverUpdated?.();
      onOpenChange(false);
    } catch (e: any) {
      setError(e.message || "清除失败");
    } finally {
      setLoading(false);
    }
  };

  if (!open) return null;

  return (
    <Modal
      open={open}
      label={`设置媒体库封面 — ${library.name}`}
      onClose={() => onOpenChange(false)}
      width="2xl"
    >
      <div className="p-6">
        <div className="flex items-center justify-between pb-4 border-b border-white/10">
          <h2 className="text-lg font-semibold text-white">
            设置媒体库封面 — {library.name}
          </h2>
          <button
            type="button"
            onClick={() => onOpenChange(false)}
            className="rounded-full p-1 text-white/50 hover:bg-white/10 hover:text-white"
          >
            ✕
          </button>
        </div>

        {/* 实时预览区 (16:9) */}
        <div className="relative mt-4 aspect-video w-full overflow-hidden rounded-xl border border-white/10 bg-[#06080e]">
          <img
            src={previewSrc || currentCoverSrc}
            alt="封面预览"
            className="size-full object-cover"
            onError={(e) => {
              (e.currentTarget as HTMLElement).style.display = "none";
            }}
          />
          {loading && (
            <div className="absolute inset-0 flex items-center justify-center bg-black/50 backdrop-blur-xs text-white text-sm">
              渲染中...
            </div>
          )}
        </div>

        {error && (
          <div className="mt-3 rounded-lg bg-red-500/10 p-2.5 text-caption text-red-400">
            {error}
          </div>
        )}

        {/* Tab 导航 */}
        <div className="mt-4 flex gap-2 border-b border-white/10 pb-2">
          <button
            type="button"
            onClick={() => setActiveTab("generate")}
            className={`rounded-lg px-3 py-1.5 text-ui font-medium transition ${
              activeTab === "generate"
                ? "bg-white/15 text-white"
                : "text-white/60 hover:text-white"
            }`}
          >
            🎨 自动生成封面
          </button>
          <button
            type="button"
            onClick={() => setActiveTab("upload")}
            className={`rounded-lg px-3 py-1.5 text-ui font-medium transition ${
              activeTab === "upload"
                ? "bg-white/15 text-white"
                : "text-white/60 hover:text-white"
            }`}
          >
            📁 本地上传替换
          </button>
        </div>

        {/* 生成控制面板 */}
        {activeTab === "generate" && (
          <div className="mt-4 space-y-3">
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className="block text-caption text-white/70 mb-1">
                  中文主标题
                </label>
                <input
                  type="text"
                  value={titleZh}
                  onChange={(e) => {
                    setTitleZh(e.target.value);
                    setPreviewSrc(null);
                  }}
                  placeholder={library.name}
                  className="w-full rounded-lg border border-white/10 bg-white/5 px-3 py-2 text-ui text-white outline-none focus:border-white/30"
                />
              </div>
              <div>
                <label className="block text-caption text-white/70 mb-1">
                  英文副标题 (可选)
                </label>
                <input
                  type="text"
                  value={titleEn}
                  onChange={(e) => {
                    setTitleEn(e.target.value);
                    setPreviewSrc(null);
                  }}
                  placeholder="例如: CHINESE MOVIES"
                  className="w-full rounded-lg border border-white/10 bg-white/5 px-3 py-2 text-ui text-white outline-none focus:border-white/30"
                />
              </div>
            </div>

            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className="block text-caption text-white/70 mb-1">
                  排版风格
                </label>
                <select
                  value={style}
                  onChange={(e: any) => {
                    setStyle(e.target.value);
                    setPreviewSrc(null);
                  }}
                  className="w-full rounded-lg border border-white/10 bg-[#161a29] px-3 py-2 text-ui text-white outline-none focus:border-white/30"
                >
                  <option value="macaron_card_single">
                    单图倾斜微卡片 (Macaron Single)
                  </option>
                  <option value="multi_poster_pile">
                    多图错落折角堆叠 (Multi-Poster)
                  </option>
                </select>
              </div>
              <div>
                <label className="block text-caption text-white/70 mb-1">
                  背景底色模式
                </label>
                <select
                  value={bgMode}
                  onChange={(e: any) => {
                    setBgMode(e.target.value);
                    setPreviewSrc(null);
                  }}
                  className="w-full rounded-lg border border-white/10 bg-[#161a29] px-3 py-2 text-ui text-white outline-none focus:border-white/30"
                >
                  <option value="auto">自适应提取 (模糊底图 + 主色融合)</option>
                  <option value="gradient_dark">暗夜极光微光渐变</option>
                  <option value="solid_dark">深色纯色微光 (#121824)</option>
                </select>
              </div>
            </div>

            <div className="flex items-center justify-between pt-2">
              <button
                type="button"
                onClick={handleDeleteCover}
                disabled={loading}
                className="rounded-lg px-3 py-2 text-caption font-medium text-red-400 hover:bg-red-500/10"
              >
                恢复默认
              </button>
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => handleGenerate(true)}
                  disabled={loading}
                  className="rounded-lg border border-white/15 px-4 py-2 text-ui font-medium text-white hover:bg-white/10"
                >
                  🎲 预览效果
                </button>
                <button
                  type="button"
                  onClick={() => handleGenerate(false)}
                  disabled={loading}
                  className="rounded-lg bg-blue-600 px-4 py-2 text-ui font-medium text-white hover:bg-blue-500"
                >
                  保存设为封面
                </button>
              </div>
            </div>
          </div>
        )}

        {/* 本地上传面板 */}
        {activeTab === "upload" && (
          <div className="mt-4 py-6 text-center">
            <label className="flex flex-col items-center justify-center border-2 border-dashed border-white/15 rounded-xl p-8 cursor-pointer hover:border-white/30 transition">
              <span className="text-2xl mb-2">📸</span>
              <span className="text-ui text-white font-medium">
                点击选取或拖拽本地横版封面图片 (16:9 推荐)
              </span>
              <span className="text-caption text-white/50 mt-1">
                支持 JPG / PNG / WebP 格式
              </span>
              <input
                type="file"
                accept="image/jpeg,image/png,image/webp"
                onChange={handleFileUpload}
                className="hidden"
              />
            </label>
          </div>
        )}
      </div>
    </Modal>
  );
}

