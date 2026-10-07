import { useParams } from "react-router-dom";

import { LibraryDetailView } from "@/components/library-detail-view";

/** 单库页（/library/[id]）：库信息头部 + 库内作品海报墙。 */
export default function LibraryDetailPage() {
  const { id = "" } = useParams();
  return (
    <div className="flex h-full flex-col">
      <LibraryDetailView libraryId={id} />
    </div>
  );
}
