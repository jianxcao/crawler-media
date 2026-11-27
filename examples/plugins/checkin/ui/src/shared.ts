import type {
  BrowserAvailability,
  OptionView,
  PluginHostSDK,
  Receipt,
} from "@crawler-media/plugin-ui-sdk";

export type ReceiptTone = "ok" | "warn" | "danger" | "neutral";

export function receiptTone(state: Receipt["state"]): ReceiptTone {
  switch (state) {
    case "completed":
    case "already-completed":
      return "ok";
    case "auth-expired":
    case "challenge":
      return "warn";
    case "failed":
      return "danger";
    default:
      return "neutral";
  }
}

export function receiptLabel(state: Receipt["state"]): string {
  switch (state) {
    case "completed":
      return "签到成功";
    case "already-completed":
      return "今日已签";
    case "auth-expired":
      return "会话失效";
    case "challenge":
      return "需人机校验";
    case "failed":
      return "签到失败";
    default:
      return "待确认";
  }
}

export function formatIsoShort(iso: string): string {
  if (!iso) return "-";
  return iso.replace("T", " ").replace("Z", " UTC").slice(0, 19);
}

export async function loadCheckinContext(
  host: PluginHostSDK,
  selectedSiteId?: string,
): Promise<{
  sites: OptionView[];
  activeSiteId: string;
  receipts: Receipt[];
  availability: BrowserAvailability | null;
}> {
  const sitePage = await host.catalogs.list("sites", { limit: 100 });
  const sites = sitePage.items;
  const activeSiteId =
    selectedSiteId && sites.some((s) => s.id === selectedSiteId)
      ? selectedSiteId
      : (sites[0]?.id ?? "");

  if (!activeSiteId) {
    return { sites, activeSiteId: "", receipts: [], availability: null };
  }

  const [receiptPage, availability] = await Promise.all([
    host.query("checkin.receipts", { site_id: activeSiteId, limit: 30 }),
    host.browser
      .availability({ site_id: activeSiteId, purpose: "maintenance" })
      .catch(() => null),
  ]);

  return {
    sites,
    activeSiteId,
    receipts: receiptPage.items,
    availability,
  };
}
