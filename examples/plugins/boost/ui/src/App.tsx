import "./style.css";

import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { Card } from "@crawler-media/plugin-ui-kit";
import BoostPage from "./Page";

export default function BoostApp(props: PluginSurfaceProps) {
  return (
    <div className="cm-boost-shell">
      <Card
        title="Boost Manager"
        description="全量刷流配额监控、单任务清理与安全删文件确认工作台。"
      >
        <BoostPage {...props} />
      </Card>
    </div>
  );
}
