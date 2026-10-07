import { useParams } from "react-router-dom";

import { DiscoveredPersonDetailView } from "@/components/discovered-person-detail-view";
import { NotFound } from "@/src/not-found";

/** 影人页（/discover/people/[id]）：TMDB combined credits 完整履历。 */
export default function DiscoveredPersonPage() {
  const { id = "" } = useParams();
  if (!/^\d+$/.test(id)) return <NotFound />;
  return <DiscoveredPersonDetailView key={`person:${id}`} tmdbPersonId={id} />;
}
