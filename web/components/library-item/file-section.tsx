"use client";

import { useState } from "react";

import { ChevronRightIcon, TrashIcon } from "@/components/icons";
import { Tooltip } from "@/components/tooltip";
import type { LibraryItemFile } from "@/lib/api/libraries";
import {
  fileCodecLabel,
  fileDirectory,
  fileHdrLabel,
  fileNameOf,
  fileResolutionLabel,
} from "@/lib/library-item-format";

/**
 * 文件区：详情正文的最后一块。默认只露出文件名，点击后按物理文件展开尺寸、视频与码率。
 */
export function FileSection({
  files,
  title,
  onDeleteFile,
}: {
  files: LibraryItemFile[];
  title: string;
  onDeleteFile?: (file: LibraryItemFile) => void;
}) {
  return (
    <section>
      <h2 className="mb-3 flex items-center gap-2 text-ui font-medium tracking-[-0.01em] text-[var(--text-muted)]">
        <span>
          {title}{" "}
          <span className="tnum text-caption font-normal text-[var(--text-faint)]">
            {files.length}
          </span>
        </span>
      </h2>
      <div className="overflow-hidden rounded-xl border border-white/[0.04] bg-white/[0.015]">
        {files.map((file) => (
          <FileRow
            key={file.file_id}
            file={file}
            onDelete={onDeleteFile ? () => onDeleteFile(file) : undefined}
          />
        ))}
      </div>
    </section>
  );
}

function FileRow({
  file,
  onDelete,
}: {
  file: LibraryItemFile;
  onDelete?: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const resolvedRes = fileResolutionLabel(file);
  const resolvedHdr = fileHdrLabel(file);
  const resolvedCodec = fileCodecLabel(file);
  const pictureInfo = [resolvedRes, resolvedHdr]
    .filter((value): value is string => Boolean(value))
    .join(" · ");
  const detailsId = `file-details-${file.file_id}`;
  const name = fileNameOf(file.path);

  return (
    <div className="border-b border-white/[0.035] last:border-b-0">
      <div className="group/filerow flex items-center gap-2 px-4 py-2.5 transition-colors hover:bg-white/[0.025]">
        <button
          type="button"
          aria-expanded={expanded}
          aria-controls={detailsId}
          onClick={() => setExpanded((value) => !value)}
          className="flex min-w-0 flex-1 items-center gap-2.5 text-left text-[var(--text-muted)] outline-none transition-colors hover:text-[var(--text)] focus-visible:text-[var(--text)]"
        >
          <ChevronRightIcon
            className={`size-3.5 shrink-0 transition-transform ${expanded ? "rotate-90" : ""}`}
          />
          <Tooltip
            content={
              <span className="tnum break-all font-mono text-caption leading-5">
                {file.path}
              </span>
            }
            maxWidth={520}
          >
            <span className="min-w-0 truncate text-sub">{name}</span>
          </Tooltip>
        </button>
        {onDelete && (
          <button
            type="button"
            aria-label="删除此文件"
            title="删除此文件"
            onClick={onDelete}
            className="touch-reveal touch-target shrink-0 rounded-md p-1.5 text-[var(--text-faint)] opacity-0 transition group-hover/filerow:opacity-100 hover:bg-white/[0.05] hover:text-[#ff9f9f]"
          >
            <TrashIcon className="size-4" />
          </button>
        )}
      </div>
      {expanded && (
        <div
          id={detailsId}
          className="border-t border-white/[0.035] bg-black/[0.08] px-10 py-4 max-md:px-4"
        >
          <dl className="grid max-w-2xl grid-cols-[72px_minmax(0,1fr)] gap-x-5 gap-y-3 text-sub max-md:grid-cols-[64px_minmax(0,1fr)]">
            <dt className="text-[var(--text-faint)]">保存目录</dt>
            <dd className="min-w-0 break-all font-mono text-caption leading-5 text-[var(--text-muted)]">
              {fileDirectory(file.path)}
            </dd>
            <dt className="text-[var(--text-faint)]">画面规格</dt>
            <dd className="tnum text-[var(--text-muted)]">
              {pictureInfo || "未能探测（文件不可达或尚未扫描）"}
            </dd>
            <dt className="text-[var(--text-faint)]">视频编码</dt>
            <dd className="tnum text-[var(--text-muted)]">{resolvedCodec || "尚未探测"}</dd>
            <dt className="text-[var(--text-faint)]">片源</dt>
            <dd className="text-[var(--text-muted)]">{file.quality_source || "未识别"}</dd>
          </dl>
        </div>
      )}
    </div>
  );
}
