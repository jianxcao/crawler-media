import { useParams } from "react-router-dom";

import { CollectionGridView } from "@/components/collection-grid-view";
import { NotFound } from "@/src/not-found";

/** 通用片单落地页：路由参数只负责恢复服务端签发的稳定片单引用。 */
export default function DiscoveryCollectionPage() {
  const { type = "", provider = "", collectionId = "" } = useParams();
  if (
    (type !== "movie" && type !== "tv") ||
    (provider !== "tmdb" && provider !== "douban") ||
    !collectionId
  ) {
    return <NotFound />;
  }
  const collectionRef = `${provider}:${type}:${collectionId}`;
  return (
    <div className="flex h-full flex-col">
      <CollectionGridView key={collectionRef} collectionRef={collectionRef} />
    </div>
  );
}
