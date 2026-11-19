"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { Link, useNavigate } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { ContentEmptyState } from "@/components/content-empty-state";
import { useConfirm, useToast } from "@/components/feedback";
import { PlusIcon } from "@/components/icons";
import { DuplicatesPanel } from "@/components/library-manage/duplicates-panel";
import {
  consolidateLibrary,
  previewOrganize,
  reconcileLibrary,
} from "@/components/library-manage/library-maintenance-actions";
import { LibraryManageDialogs } from "@/components/library-manage/library-manage-dialogs";
import {
  LibraryManageFilterToolbar,
  LibraryManageHeader,
} from "@/components/library-manage/library-manage-header";
import {
  LibraryManageRow,
  type LibraryRowActions,
  type LibraryRowDrag,
} from "@/components/library-manage-row";
import { LibraryRemovalNotice } from "@/components/library-removal-notice";
import { PageNav } from "@/components/page-nav";
import {
  listLibraries,
  type MediaLibrary,
  type OrganizePreviewResult,
  reorderLibraries,
  setDefaultLibrary,
  startLibraryMetadataRefresh,
  startLibraryScan,
} from "@/lib/api/libraries";
import { refreshLibraryConfirm, scanLibraryConfirm } from "@/lib/library-confirm";
import {
  EMPTY_FILTER,
  filterIsActive,
  filterLibraries,
  type LibraryFilter,
  type LibraryFocus,
  moveInList,
  summarizeLibraries,
} from "@/lib/library-manage";
import { computeDropPosition } from "@/lib/library-manage-helpers";
import { routingOverlapWarnings } from "@/lib/library-routing-warnings";
import type { LibraryKind } from "@/lib/media-types";
import { usePermissions } from "@/lib/permissions";
import { useLibraryRemoval } from "@/lib/use-library-removal";
import { useIsMobile } from "@/lib/use-media-query";
import { useVisiblePolling } from "@/lib/use-visible-polling";

/** 指针落在目标行的上半还是下半：决定放到它之前还是之后。 */
function dropPosition(e: React.DragEvent): "before" | "after" {
  const rect = e.currentTarget.getBoundingClientRect();
  return computeDropPosition(e.clientY, rect.top, rect.height);
}

/**
 * 媒体库管理页（/library/manage）：一库一行的纵向列表，库多了只是变长。
 */
export function LibraryManageView() {
  const { canManageLibraries } = usePermissions();
  const navigate = useNavigate();
  const confirm = useConfirm();
  const toast = useToast();
  const isMobile = useIsMobile();

  const [tab, setTab] = useState<"libraries" | "duplicates">("libraries");
  const [libraries, setLibraries] = useState<MediaLibrary[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [filter, setFilter] = useState<LibraryFilter>(EMPTY_FILTER);
  const [editing, setEditing] = useState<MediaLibrary | "new" | null>(null);
  const [coverModalLibrary, setCoverModalLibrary] = useState<MediaLibrary | null>(null);
  const [mediaInfoLibrary, setMediaInfoLibrary] = useState<MediaLibrary | null>(null);
  const [reorderOpen, setReorderOpen] = useState(false);
  const [organizing, setOrganizing] = useState<{
    library: MediaLibrary;
    preview: OrganizePreviewResult;
  } | null>(null);
  const [revealId, setRevealId] = useState<string | null>(null);

  const reloadSeq = useRef(0);
  const reload = useCallback(() => {
    const seq = ++reloadSeq.current;
    listLibraries(undefined, { manage: true })
      .then((libs) => {
        if (seq !== reloadSeq.current) return;
        setFailed(false);
        const snapshot = JSON.stringify(libs);
        setLibraries((prev) => (prev && JSON.stringify(prev) === snapshot ? prev : libs));
      })
      .catch(() => {
        if (seq === reloadSeq.current) setFailed(true);
      });
  }, []);

  const removal = useLibraryRemoval(libraries, (library) => {
    toast.success(`已删除媒体库「${library.name}」`);
    setLibraries((prev) => prev?.filter((entry) => entry.id !== library.id) ?? null);
    reload();
  });

  useEffect(() => {
    reload();
  }, [reload]);

  useEffect(() => {
    if (!canManageLibraries) return;
    const params = new URLSearchParams(window.location.search);
    if (params.get("create") !== "1") return;
    setEditing("new");
    params.delete("create");
    const rest = params.toString();
    window.history.replaceState(null, "", `${window.location.pathname}${rest ? `?${rest}` : ""}`);
  }, [canManageLibraries]);

  useVisiblePolling(reload, 30_000);

  useEffect(() => {
    if (revealId === null) return;
    document
      .querySelector(`[data-library-row="${revealId}"]`)
      ?.scrollIntoView({ behavior: "smooth", block: "center" });
    setRevealId(null);
  }, [revealId, libraries]);

  const warnings = useMemo(() => routingOverlapWarnings(libraries ?? []), [libraries]);
  const visible = useMemo(() => filterLibraries(libraries ?? [], filter), [libraries, filter]);
  const summary = useMemo(() => summarizeLibraries(libraries ?? []), [libraries]);
  const toggleFocus = (focus: LibraryFocus) => {
    setFilter((f) => ({ ...f, focus: f.focus === focus ? null : focus }));
  };
  const kindCounts = useMemo(() => {
    const counts = new Map<LibraryKind, number>();
    for (const l of libraries ?? []) counts.set(l.kind, (counts.get(l.kind) ?? 0) + 1);
    return counts;
  }, [libraries]);

  const run = useCallback(
    (action: Promise<unknown>, done?: string) => {
      void action
        .then(() => {
          reload();
          if (done) toast.success(done);
        })
        .catch((e) => toast.error((e as Error).message));
    },
    [reload, toast],
  );

  const commitOrder = useCallback(
    (next: readonly MediaLibrary[]) => {
      const prev = libraries;
      setLibraries([...next]);
      void reorderLibraries(next.map((l) => l.id))
        .then(() => {
          reload();
          toast.success("顺序已更新，首页「我的媒体库」同步生效");
        })
        .catch((e) => {
          setLibraries(prev);
          toast.error((e as Error).message);
        });
    },
    [libraries, reload, toast],
  );

  const moveLibrary = useCallback(
    (libraryId: string, to: number) => {
      if (!libraries) return;
      const from = libraries.findIndex((l) => l.id === libraryId);
      const next = moveInList(libraries, from, to);
      if (next !== libraries) commitOrder(next);
    },
    [libraries, commitOrder],
  );

  const actions: LibraryRowActions = useMemo(
    () => ({
      onToggleScan: (library) => {
        void confirm(scanLibraryConfirm(library.name)).then((ok) => {
          if (ok) run(startLibraryScan(library.id), `已开始扫描「${library.name}」`);
        });
      },
      onOpenPending: (library) => {
        navigate(`/library/${library.id}?pending=1`);
      },
      onToggleRefresh: (library) => {
        void confirm(refreshLibraryConfirm(library.name)).then((ok) => {
          if (ok) run(startLibraryMetadataRefresh(library.id));
        });
      },
      onMediaInfo: setMediaInfoLibrary,
      onEdit: (library) => setEditing(library),
      onSetDefault: (library) => {
        void confirm({
          title: `将「${library.name}」设为默认库？`,
          description: `设为默认后，未指定库的 ${library.kind === "movie" ? "电影" : "剧集"} 订阅和自动入库将优先归属到此库。原默认库将变为普通库。`,
          confirmLabel: "设为默认",
          tone: "default",
        }).then((ok) => {
          if (ok) {
            void setDefaultLibrary(library.id)
              .then(() => {
                toast.success(`已将「${library.name}」设为默认库`);
                reload();
              })
              .catch((e: Error) => toast.error(e.message));
          }
        });
      },
      onDelete: (library) => {
        void confirm({
          title: `删除媒体库「${library.name}」？`,
          description: "将停止该库相关任务，清除扫描记录与探测缓存。磁盘文件、NFO 和图片不会删除。若作品在其他媒体库中没有文件，也未被合集或订阅保留，其观看进度、已看状态和收藏将一并清除，重新扫描不会恢复这些记录。",
          confirmLabel: "删除媒体库",
          tone: "danger",
        }).then((ok) => {
          if (ok) {
            removal.remove(library);
          }
        });
      },
      onOrganize: (library) => {
        void previewOrganize(library, toast).then((preview) => {
          if (preview) setOrganizing({ library, preview });
        });
      },
      onReconcile: (library) => {
        void reconcileLibrary(library, toast, confirm);
      },
      onConsolidate: (library) => {
        void consolidateLibrary(library, toast, confirm);
      },
      onCoverModal: (library: MediaLibrary) => {
        setCoverModalLibrary(library);
      },
      onReorder: isMobile ? () => setReorderOpen(true) : undefined,
    }),
    [confirm, isMobile, navigate, reload, run, toast, removal],
  );

  // —— 拖拽排序（桌面端、未筛选时）——
  const [dragId, setDragId] = useState<string | null>(null);
  const [over, setOver] = useState<{ id: string; pos: "before" | "after" } | null>(null);
  const dragEnabled = !isMobile && !filterIsActive(filter) && (libraries?.length ?? 0) > 1;
  const dragFor = (library: MediaLibrary): LibraryRowDrag | null => {
    if (!dragEnabled || !libraries) return null;
    const index = libraries.findIndex((l) => l.id === library.id);
    return {
      dragging: dragId === library.id,
      over: over?.id === library.id && dragId !== library.id ? over.pos : null,
      onDragStart: (e) => {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", String(library.id));
        const row = (e.currentTarget as HTMLElement).closest("[data-library-row]");
        if (row instanceof HTMLElement) e.dataTransfer.setDragImage(row, 24, row.offsetHeight / 2);
        setDragId(library.id);
      },
      onDragOver: (e) => {
        if (dragId === null) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        const pos = dropPosition(e);
        if (over?.id !== library.id || over.pos !== pos) setOver({ id: library.id, pos });
      },
      onDrop: (e) => {
        e.preventDefault();
        if (dragId !== null && dragId !== library.id) {
          const from = libraries.findIndex((l) => l.id === dragId);
          const before = dropPosition(e) === "before";
          const to = before ? (from < index ? index - 1 : index) : from < index ? index : index + 1;
          moveLibrary(dragId, to);
        }
        setDragId(null);
        setOver(null);
      },
      onDragEnd: () => {
        setDragId(null);
        setOver(null);
      },
      onMoveKey: (offset) => moveLibrary(library.id, index + offset),
    };
  };

  if (!canManageLibraries) {
    return (
      <div className="scroll-thin scroll-safe flex-1 overflow-y-auto pb-10">
        <PageNav title="媒体库管理" fallback={{ label: "媒体库", href: "/library" }} />
        <ContentEmptyState
          variant="library"
          title="没有管理权限"
          description="媒体库的创建、扫描与排序由管理员负责；你可以回到媒体库继续浏览。"
          action={
            <Link to={"/library"} className="btn-glass px-4 py-2 text-ui font-medium">
              返回媒体库
            </Link>
          }
        />
      </div>
    );
  }

  return (
    <div className="scroll-thin scroll-safe flex-1 overflow-y-auto pb-10">
      <PageNav title="媒体库管理" fallback={{ label: "媒体库", href: "/library" }} />

      <LibraryManageHeader
        libraries={libraries}
        summary={summary}
        filter={filter}
        tab={tab}
        onCreate={() => setEditing("new")}
        onToggleFocus={toggleFocus}
        onTabChange={setTab}
      />

      {tab === "libraries" && (
        <>
          {failed && libraries !== null && (
            <div className="mx-6 mt-4 rounded-xl border border-amber-400/25 bg-amber-500/10 px-4 py-3 text-sub text-amber-200 max-md:mx-4">
              与后端通信失败，正在自动重试；下方显示的是最近一次成功加载的数据
            </div>
          )}

          {libraries === null && !failed && (
            <div className="mt-16 flex items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
              <BrandLoader className="size-5" />
              正在加载媒体库…
            </div>
          )}
          {failed && libraries === null && (
            <div className="mt-16 flex flex-col items-center gap-3 text-center">
              <p className="text-ui text-[var(--text-muted)]">媒体库加载失败</p>
              <button
                type="button"
                onClick={reload}
                className="btn-glass px-4 py-2 text-ui font-medium text-[var(--text)]"
              >
                重试
              </button>
            </div>
          )}

          {libraries !== null && libraries.length === 0 && (
            <ContentEmptyState
              variant="library"
              title="为收藏准备一个家"
              description="创建电影库或剧集库，选好根目录后，订阅完成的内容会自动整理到这里。"
              action={
                <button
                  type="button"
                  onClick={() => setEditing("new")}
                  className="btn-accent flex items-center gap-1 rounded-full py-2 pl-3 pr-4 text-ui font-semibold"
                >
                  <PlusIcon className="size-4" />
                  创建第一个媒体库
                </button>
              }
            />
          )}

          {libraries !== null && libraries.length > 0 && (
            <>
              <LibraryManageFilterToolbar
                totalLibraries={libraries.length}
                kindCounts={kindCounts}
                filter={filter}
                onFilterChange={setFilter}
              />

              {warnings.map((w) => (
                <div
                  key={w}
                  className="mx-6 mt-4 rounded-xl border border-amber-400/25 bg-amber-500/10 px-4 py-3 text-sub leading-relaxed text-amber-200 max-md:mx-4"
                >
                  {w}
                </div>
              ))}

              <div className="mx-6 mt-4 overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] max-md:mx-4">
                {visible.length === 0 ? (
                  <div className="px-4 py-10 text-center text-ui text-[var(--text-muted)]">
                    没有符合条件的媒体库
                    <button
                      type="button"
                      onClick={() => setFilter(EMPTY_FILTER)}
                      className="ml-2 text-[var(--info)] hover:underline"
                    >
                      清除筛选
                    </button>
                  </div>
                ) : (
                  <div role="list" aria-label="媒体库列表" className="divide-y divide-white/[0.06]">
                    {visible.map((library) => (
                      <div key={library.id}>
                        <LibraryManageRow
                          library={library}
                          actions={actions}
                          removalBlocked={Boolean(
                            removal.states[library.id]?.pending ||
                              removal.states[library.id]?.operation ||
                              library.removal,
                          )}
                          drag={
                            removal.states[library.id]?.pending || library.removal
                              ? null
                              : dragFor(library)
                          }
                        />
                        <div className="px-4">
                          <LibraryRemovalNotice
                            library={library}
                            state={removal.states[library.id]}
                            onRetry={(operation) =>
                              operation ? removal.resume(library, operation) : removal.remove(library)
                            }
                          />
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>

              <p className="mx-6 mt-3 text-caption text-[var(--text-faint)] max-md:mx-4">
                {isMobile
                  ? "顺序即首页「我的媒体库」的展示顺序，在 ··· 菜单里「调整顺序」"
                  : dragEnabled
                    ? "把指针停在行上，拖动行首的把手调整首页「我的媒体库」的展示顺序，松手即保存"
                    : filterIsActive(filter)
                      ? "清除筛选后可拖拽排序"
                      : ""}
              </p>
            </>
          )}
        </>
      )}

      {tab === "duplicates" && <DuplicatesPanel />}

      <LibraryManageDialogs
        libraries={libraries}
        editing={editing}
        onCloseEditing={() => setEditing(null)}
        onSavedLibrary={(saved, isNew) => {
          if (isNew) setRevealId(saved.id);
          reload();
        }}
        coverModalLibrary={coverModalLibrary}
        onCloseCoverModal={() => setCoverModalLibrary(null)}
        mediaInfoLibrary={mediaInfoLibrary}
        onCloseMediaInfo={() => setMediaInfoLibrary(null)}
        organizing={organizing}
        onCloseOrganizing={() => setOrganizing(null)}
        reorderOpen={reorderOpen}
        onCloseReorder={() => setReorderOpen(false)}
        onSubmitReorder={commitOrder}
        reload={reload}
      />
    </div>
  );
}
