"use client";

import type { MediaLibrary } from "@/lib/api/libraries";

import { CreateLibraryDialog } from "./library-form/create-library-dialog";
import { EditLibraryDialog } from "./library-form/edit-library-dialog";

/**
 * 媒体库表单弹窗（新建向导 / 编辑分区），从 library-view.tsx 拆出：
 * 首页只做浏览入口，建库与编辑落在管理页，
 * 单库页头部的「编辑库」也用这里的 LibraryFormDialog。
 */
export function LibraryFormDialog({
  state,
  onClose,
  onSaved,
}: {
  /** "new"=新增；库对象=编辑；null=关闭 */
  state: MediaLibrary | "new" | null;
  onClose: () => void;
  /** 保存成功回调，带上服务端返回的库（新建时调用方据此把新卡片滚进视野） */
  onSaved: (saved: MediaLibrary) => void;
}) {
  if (state === null) return null;
  if (state === "new") return <CreateLibraryDialog onClose={onClose} onSaved={onSaved} />;
  return <EditLibraryDialog key={state.id} library={state} onClose={onClose} onSaved={onSaved} />;
}
