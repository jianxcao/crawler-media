import { useParams } from "react-router-dom";

import { PluginDetailView } from "@/components/plugins/PluginDetailView";
import { NotFound } from "@/src/not-found";

export default function PluginDetailPage() {
  const { pluginId = "" } = useParams();
  if (!pluginId) return <NotFound />;
  return <PluginDetailView key={pluginId} pluginId={pluginId} />;
}
