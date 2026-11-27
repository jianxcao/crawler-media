import { useEffect, useState } from "react";
import { Navigate, useParams, useSearchParams } from "react-router-dom";

import { MediaDetailView } from "@/components/media-detail-view";
import { getLibraryItemDetail, listLibraries } from "@/lib/api/libraries";
import type { MediaSource } from "@/lib/media-types";
import { NotFound } from "@/src/not-found";

function MediaItemRedirect({ id }: { id: string }) {
  const [state, setState] = useState<
    { status: "loading" } | { status: "found"; libraryId: string } | { status: "not-found" }
  >({ status: id ? "loading" : "not-found" });

  useEffect(() => {
    if (!id) {
      setState({ status: "not-found" });
      return;
    }
    let cancelled = false;
    const ac = new AbortController();
    setState({ status: "loading" });

    void (async () => {
      try {
        const libs = await listLibraries(undefined, { signal: ac.signal });
        for (const lib of libs) {
          if (cancelled) return;
          try {
            const detail = await getLibraryItemDetail(lib.id, id, { signal: ac.signal });
            if (cancelled) return;
            if (detail && detail.media_item_id) {
              setState({ status: "found", libraryId: lib.id });
              return;
            }
          } catch {
            /* item not in this library */
          }
        }
        if (!cancelled) setState({ status: "not-found" });
      } catch {
        if (!cancelled) setState({ status: "not-found" });
      }
    })();

    return () => {
      cancelled = true;
      ac.abort();
    };
  }, [id]);

  if (state.status === "loading") {
    return <div className="p-8 text-sub text-[var(--text-muted)]">正在定位媒体条目…</div>;
  }
  if (state.status === "found") {
    return (
      <Navigate
        to={`/library/${encodeURIComponent(state.libraryId)}/item/${encodeURIComponent(id)}`}
        replace
      />
    );
  }
  return <NotFound />;
}

/** 影片详情（/media/movie|tv/[id]，以及 /media/item/[id] 自动跳转媒体库条目）：词条信息 + 剧照 + 相似推荐。 */
export default function MediaDetailPage() {
  const { type = "", id = "" } = useParams();
  const [searchParams] = useSearchParams();
  const source = (searchParams.get("source") as MediaSource) || "tmdb";
  if (type === "item") return <MediaItemRedirect key={id} id={id} />;
  if (type !== "movie" && type !== "tv") return <NotFound />;
  // key 按影片切换强制重建：在详情页内点「相似推荐」跳详情时回到顶部、重拉数据
  return <MediaDetailView key={`${source}:${type}:${id}`} type={type} id={id} source={source} />;
}
