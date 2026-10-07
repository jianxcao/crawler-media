import { useParams } from "react-router-dom";
import { CollectionDetailPage } from "@/components/collections-detail-page";

export const metadata = { title: "合集" };

export default function Page() {
  const params = useParams<{ id: string }>();
  return <CollectionDetailPage idPromise={Promise.resolve({ id: params.id ?? "" })} />;
}
