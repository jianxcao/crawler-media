"use client";

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import { MoreIcon } from "@/components/icons";
import { PAGE_NAV_BUTTON_CLASS } from "@/components/page-nav";
import { type PhotoWallDensity } from "@/components/wall";
import { WallPrefItems } from "@/components/video-gallery";
import { type MediaLibrary, setDefaultLibrary } from "@/lib/api/libraries";

/* —— 库操作折叠菜单：待处理 / 扫描库 / 整理文件名 / 刷新元数据 / 编辑库 全收进 ⋯，头部不再摆一排按钮。
 *  三个长任务在菜单里就能看进度并原地停止，运行状态另由头部胶囊常驻呈现。 */

export interface LibraryActionsMenuProps {
  /** 当前媒体库对象 */
  library: MediaLibrary;
  /** 媒体库管理权限：没有时菜单只剩图片库的相册墙密度 */
  canManage: boolean;
  toast: { success: (message: string) => void; error: (message: string) => void };
  reload: () => void;
  onToggleScan: () => void;
  /** 待处理总件数（缺失 + 待识别）；0 也照常给入口，见下方说明 */
  pendingCount: number;
  onOpenPending: () => void;
  onToggleMetaRefresh: () => void;
  onMediaInfo: () => void;
  onEdit: () => void;
  /**
   * 调用方仍传着「设为默认库」的完整实现（含二次确认），但菜单里目前没有这一项。
   * 行为留在调用处、参数留在契约里是刻意的：恢复入口时不必重写这段流程。
   */
  onSetDefault?: () => void;
  onDelete: () => void;
  onCoverModal?: () => void;
  /** 图片库：相册墙的密度（个人偏好，与管理权无关）；不传不渲染这一组 */
  density?: PhotoWallDensity;
  onDensityChange?: (next: PhotoWallDensity) => void;
  /** 图床浏览的开关：当前在不在图床模式；不传不渲染这一项（图片库没有） */
  galleryMode?: boolean;
  onGalleryModeChange?: (next: boolean) => void;
  /** 图床浏览模式：是否按作品分段；不传不渲染这一项（海报墙与图片库都没有分组一说） */
  grouped?: boolean;
  onGroupedChange?: (next: boolean) => void;
}

export function LibraryActionsMenu({
  library,
  canManage,
  toast,
  reload,
  onToggleScan,
  pendingCount,
  onOpenPending,
  onToggleMetaRefresh,
  onMediaInfo,
  onEdit,
  onDelete,
  onCoverModal,
  density,
  onDensityChange,
  galleryMode,
  onGalleryModeChange,
  grouped,
  onGroupedChange,
}: LibraryActionsMenuProps) {
  // 与站点配置一致用 Radix DropdownMenu：Portal 到 body + 碰撞检测，
  // 不会被头部容器裁切；开合/外部点击/键盘导航全交给 Radix。
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2 text-ui font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)] " +
    "data-[disabled]:pointer-events-none data-[disabled]:opacity-40";

  return (
    <DropdownMenu.Root modal={false}>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          aria-label="更多操作"
          // 长在顶栏里，与返回键并排：共用顶栏控件形状，不再是页面里的胶囊按钮
          className={`${PAGE_NAV_BUTTON_CLASS} relative`}
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
            <>
              {/* 待处理清单的**常驻**入口。头部胶囊是条件渲染的（有待办才出现），
                  光靠它意味着清单一空抽屉就整个不可达——「已忽略」更是从来没有
                  自己的胶囊，把待识别全忽略掉之后就再也回不去了。这里恒在，
                  计数为 0 时也不隐藏：归档随时可查，空清单本身也是有效答案 */}
              <DropdownMenu.Item onSelect={onOpenPending} className={itemClass}>
                待处理{pendingCount > 0 ? ` ${pendingCount}` : ""}
              </DropdownMenu.Item>
              <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />
              {/* 新契约无扫描进行中状态：入口只有「扫描库」一个形态 */}
              <DropdownMenu.Item onSelect={onToggleScan} className={itemClass}>
                扫描库
              </DropdownMenu.Item>
              <DropdownMenu.Item onSelect={onToggleMetaRefresh} className={itemClass}>
                刷新元数据
              </DropdownMenu.Item>
              <DropdownMenu.Item onSelect={onMediaInfo} className={itemClass}>
                媒体信息抓取
              </DropdownMenu.Item>
              <DropdownMenu.Item onSelect={onEdit} className={itemClass}>
                编辑库
              </DropdownMenu.Item>
              {onCoverModal && (
                <DropdownMenu.Item onSelect={onCoverModal} className={itemClass}>
                  设置封面
                </DropdownMenu.Item>
              )}
              {!library.is_default && (
                <DropdownMenu.Item
                  onSelect={() => {
                    setTimeout(async () => {
                      try {
                        await setDefaultLibrary(library.id);
                        toast.success(`已将「${library.name}」设为默认库`);
                        reload();
                        try {
                          (window as { __CM_RELOAD_HOME__?: () => void }).__CM_RELOAD_HOME__?.();
                        } catch {
                          /* Home reload is optional. */
                        }
                      } catch (e) {
                        toast.error(e instanceof Error ? e.message : "设为默认库失败");
                      }
                    }, 50);
                  }}
                  className={itemClass}
                >
                  设为默认库
                </DropdownMenu.Item>
              )}
              <DropdownMenu.Item
                onSelect={() => {
                  setTimeout(() => onDelete(), 50);
                }}
                className="glass-row nav-item cursor-pointer px-3 py-2 text-ui font-medium !text-[var(--danger)] outline-none hover:!bg-[var(--danger)]/10 hover:!text-[var(--danger)] data-[highlighted]:!bg-[var(--danger)]/10 data-[highlighted]:!text-[var(--danger)]"
              >
                删除媒体库
              </DropdownMenu.Item>
            </>
          )}
          {/* 浏览偏好收在菜单里，不占墙上的位置，选完即生效并记住。 */}
          {canManage && <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />}
          {onGalleryModeChange && (
            <DropdownMenu.Item
              onSelect={() => onGalleryModeChange(!galleryMode)}
              className={itemClass}
            >
              {galleryMode ? "回到海报墙" : "图床浏览"}
            </DropdownMenu.Item>
          )}
          <WallPrefItems
            grouped={grouped}
            onGroupedChange={onGroupedChange}
            density={density}
            onDensityChange={onDensityChange}
            itemClass={itemClass}
          />
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
