"use client";

import { useToast } from "@/components/feedback";
import { LibraryCoverModal } from "@/components/library-cover-modal";
import { LibraryFormDialog } from "@/components/library-form-dialog";
import { applyOrganize } from "@/components/library-manage/library-maintenance-actions";
import { ReorderDialog } from "@/components/library-manage/reorder-dialog";
import { LibraryMediaInfoDialog } from "@/components/library-media-info-dialog";
import { LibraryOrganizeDialog } from "@/components/library-organize-dialog";
import type { MediaLibrary, OrganizePreviewResult } from "@/lib/api/libraries";

/** 媒体库管理页挂载的所有模态弹层。 */
export function LibraryManageDialogs({
  libraries,
  editing,
  onCloseEditing,
  onSavedLibrary,
  coverModalLibrary,
  onCloseCoverModal,
  mediaInfoLibrary,
  onCloseMediaInfo,
  organizing,
  onCloseOrganizing,
  reorderOpen,
  onCloseReorder,
  onSubmitReorder,
  reload,
}: {
  libraries: MediaLibrary[] | null;
  editing: MediaLibrary | "new" | null;
  onCloseEditing: () => void;
  onSavedLibrary: (saved: MediaLibrary, isNew: boolean) => void;
  coverModalLibrary: MediaLibrary | null;
  onCloseCoverModal: () => void;
  mediaInfoLibrary: MediaLibrary | null;
  onCloseMediaInfo: () => void;
  organizing: { library: MediaLibrary; preview: OrganizePreviewResult } | null;
  onCloseOrganizing: () => void;
  reorderOpen: boolean;
  onCloseReorder: () => void;
  onSubmitReorder: (next: readonly MediaLibrary[]) => void;
  reload: () => void;
}) {
  const toast = useToast();

  return (
    <>
      {mediaInfoLibrary && (
        <LibraryMediaInfoDialog
          library={mediaInfoLibrary}
          onClose={onCloseMediaInfo}
          onSubmitted={(receipt) => {
            toast.success(`已提交 ${receipt.queued} 个文件的媒体信息抓取，跳过 ${receipt.skipped} 个`);
            reload();
          }}
        />
      )}

      <LibraryFormDialog
        state={editing}
        onClose={onCloseEditing}
        onSaved={(saved) => {
          const isNew = editing === "new";
          onCloseEditing();
          onSavedLibrary(saved, isNew);
        }}
      />

      {coverModalLibrary && (
        <LibraryCoverModal
          library={coverModalLibrary}
          open={Boolean(coverModalLibrary)}
          onOpenChange={(open) => !open && onCloseCoverModal()}
          onCoverUpdated={reload}
        />
      )}

      {organizing && libraries && (
        <LibraryOrganizeDialog
          library={organizing.library}
          libraries={libraries}
          preview={organizing.preview}
          onClose={onCloseOrganizing}
          onSubmit={(renames) => {
            const { library } = organizing;
            onCloseOrganizing();
            void applyOrganize(library, renames, toast).then(reload);
          }}
        />
      )}

      {libraries && (
        <ReorderDialog
          open={reorderOpen}
          libraries={libraries}
          onClose={onCloseReorder}
          onSubmit={(next) => {
            onCloseReorder();
            onSubmitReorder(next);
          }}
        />
      )}
    </>
  );
}
