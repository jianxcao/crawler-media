"use client";

import { ArtworkPickerDialog } from "@/components/artwork-picker-dialog";
import { ReidentifyDialog } from "@/components/reidentify-dialog";
import type { LibraryItemDetail, LibraryItemFile } from "@/lib/api/libraries";
import { invalidateLibraryDetailSnapshot } from "@/lib/library-detail-snapshot";
import { fileNameOf } from "@/lib/library-item-format";

import { DeleteDialog, DeleteFileDialog } from "./item-delete-dialogs";

export function ItemAdminModals({
  libraryId,
  mediaItemId,
  detail,
  deleteOpen,
  onCloseDelete,
  onItemDeleted,
  deleteFileTarget,
  onCloseDeleteFile,
  reidentifyOpen,
  onCloseReidentify,
  onMarkReidentifyDirty,
  artworkOpen,
  onCloseArtwork,
  onReload,
  onFileDeletedToast,
}: {
  libraryId: string;
  mediaItemId: string;
  detail: LibraryItemDetail;
  deleteOpen: boolean;
  onCloseDelete: () => void;
  onItemDeleted: () => void;
  deleteFileTarget: LibraryItemFile | null;
  onCloseDeleteFile: () => void;
  reidentifyOpen: boolean;
  onCloseReidentify: () => void;
  onMarkReidentifyDirty: () => void;
  artworkOpen: boolean;
  onCloseArtwork: () => void;
  onReload: () => void;
  onFileDeletedToast: (fileName: string) => void;
}) {
  return (
    <>
      <DeleteDialog
        open={deleteOpen}
        detail={detail}
        onClose={onCloseDelete}
        onDeleted={onItemDeleted}
        libraryId={libraryId}
      />

      {deleteFileTarget && (
        <DeleteFileDialog
          file={deleteFileTarget}
          onClose={onCloseDeleteFile}
          onDeleted={() => {
            onFileDeletedToast(fileNameOf(deleteFileTarget.path));
            invalidateLibraryDetailSnapshot(libraryId);
            onCloseDeleteFile();
            onReload();
          }}
        />
      )}

      <ReidentifyDialog
        open={reidentifyOpen}
        libraryId={libraryId}
        mediaItemId={mediaItemId}
        onClose={onCloseReidentify}
        onApplied={() => {
          invalidateLibraryDetailSnapshot(libraryId);
          onMarkReidentifyDirty();
        }}
      />

      <ArtworkPickerDialog
        open={artworkOpen}
        libraryId={libraryId}
        mediaItemId={mediaItemId}
        onClose={onCloseArtwork}
        onChanged={onReload}
      />
    </>
  );
}
