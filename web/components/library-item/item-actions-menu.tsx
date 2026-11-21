"use client";

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useNavigate } from "react-router-dom";

import { MoreIcon } from "@/components/icons";
import { PAGE_NAV_BUTTON_CLASS } from "@/components/page-nav";

/**
 * 条目操作 ⋯ 菜单：修正识别结果 / 刷新元数据 / 更换图片 / 转移 / 删除 / 清除观看记录。
 */
export function ItemActionsMenu({
  canManage,
  identifiable,
  scraped,
  readsNfo,
  scraping,
  searchHref,
  onReidentify,
  onRefreshMetadata,
  onRefreshChapters,
  onRecaptureAudio,
  onChangeArtwork,
  onDelete,
  onUpgrade,
  onClearHistory,
}: {
  canManage: boolean;
  identifiable: boolean;
  scraped: boolean;
  readsNfo: boolean;
  scraping: boolean;
  searchHref: string;
  onReidentify: () => void;
  onRefreshMetadata: () => void;
  onRefreshChapters?: () => void;
  onRecaptureAudio?: () => void;
  onChangeArtwork: () => void;
  onDelete: () => void;
  onUpgrade?: () => void;
  onClearHistory: () => void;
}) {
  const navigate = useNavigate();
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2 text-ui font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)] " +
    "data-[disabled]:pointer-events-none data-[disabled]:opacity-40";

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          aria-label="更多操作"
          className={`${PAGE_NAV_BUTTON_CLASS} relative data-[state=open]:bg-black/55 data-[state=open]:text-white`}
        >
          <MoreIcon className="size-[18px] max-md:size-[22px]" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={6}
          collisionPadding={12}
          className="menu-surface z-50 min-w-[11rem] p-1"
        >
          {canManage && (
            <DropdownMenu.Item onSelect={() => navigate(searchHref)} className={itemClass}>
              搜索资源
            </DropdownMenu.Item>
          )}
          {onUpgrade && (
            <DropdownMenu.Item onSelect={onUpgrade} className={itemClass}>
              洗版…
            </DropdownMenu.Item>
          )}
          {canManage && (
            <>
              <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />
              {identifiable && (
                <DropdownMenu.Item onSelect={onReidentify} className={itemClass}>
                  修正识别结果…
                </DropdownMenu.Item>
              )}
              <DropdownMenu.Item
                onSelect={onRefreshMetadata}
                disabled={scraping}
                className={itemClass}
              >
                {scraped
                  ? scraping
                    ? "正在刷新元数据…"
                    : "刷新元数据"
                  : scraping
                    ? readsNfo
                      ? "正在读取 NFO…"
                      : "正在生成封面…"
                    : readsNfo
                      ? "重新读取 NFO 与封面"
                      : "重新生成封面"}
              </DropdownMenu.Item>
              {onRefreshChapters && (
                <DropdownMenu.Item onSelect={onRefreshChapters} className={itemClass}>
                  重新识别片头片尾与章节
                </DropdownMenu.Item>
              )}
              {onRecaptureAudio && (
                <DropdownMenu.Item onSelect={onRecaptureAudio} className={itemClass}>
                  重新读取音频
                </DropdownMenu.Item>
              )}
              {scraped && (
                <DropdownMenu.Item onSelect={onChangeArtwork} className={itemClass}>
                  更换图片…
                </DropdownMenu.Item>
              )}
              <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />
              <DropdownMenu.Item
                onSelect={onDelete}
                className={`${itemClass} !text-[#ff9f9f] data-[highlighted]:!bg-[rgba(255,90,90,0.16)] data-[highlighted]:!text-[#ffb4b4]`}
              >
                删除影片
              </DropdownMenu.Item>
            </>
          )}
          {(canManage || onUpgrade) && (
            <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />
          )}
          <DropdownMenu.Item onSelect={onClearHistory} className={itemClass}>
            清除观看记录…
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
