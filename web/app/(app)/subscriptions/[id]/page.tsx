import { useParams, useSearchParams } from "react-router-dom";

import { SubscriptionInspectorView } from "@/components/subscription-inspector-view";

/** 订阅详情分析页（/subscriptions/[id]）：追踪明细 + 活动时间线。
 * `?upgrade-run=1` 进入即打开「洗一轮版」弹层（库详情洗版入口的并轨跳转）。 */
export default function SubscriptionDetailPage() {
  const { id = "" } = useParams();
  const [searchParams] = useSearchParams();
  return (
    <div className="flex h-full flex-col">
      <SubscriptionInspectorView
        id={id}
        autoOpenUpgradeRun={searchParams.get("upgrade-run") === "1"}
      />
    </div>
  );
}
