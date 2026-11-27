import { useParams } from "react-router-dom";

import { PluginAppPage } from "@/components/plugins/PluginAppPage";
import { NotFound } from "@/src/not-found";

export default function PluginAppRoutePage() {
  const { pluginId = "", surfaceId = "" } = useParams();
  if (!pluginId || !surfaceId) return <NotFound />;
  return <PluginAppPage key={`${pluginId}:${surfaceId}`} pluginId={pluginId} surfaceId={surfaceId} />;
}
