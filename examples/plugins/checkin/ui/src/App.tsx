import "./style.css";

import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { Card } from "@crawler-media/plugin-ui-kit";
import CheckinPage from "./Page";

export default function CheckinApp(props: PluginSurfaceProps) {
  return (
    <div className="cm-checkin-shell">
      <Card
        title="Site Check-in Studio"
        description="全站点每日签到、会话健康检查与异常人工介入工作台。"
      >
        <CheckinPage {...props} />
      </Card>
    </div>
  );
}
