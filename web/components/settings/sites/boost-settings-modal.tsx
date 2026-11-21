"use client";

import { useEffect, useState } from "react";

import { useConfirm } from "@/components/feedback";
import { Modal } from "@/components/modal";
import { setSiteRatioBoost, type Site, type SiteBoostStats } from "@/lib/api/sites";
import { formatBytes } from "@/lib/format";

export const GIB = 1024 ** 3;

/* —— 刷流设置弹窗：预算 + 汰换保留期同窗设置，开启确认与运行中调整共用 —— */

export function BoostSettingsModal({
  open,
  mode,
  siteName,
  site,
  boost,
  onClose,
  onChanged,
}: {
  open: boolean;
  /** enable=开启前的二次确认（讲清将发生什么），adjust=运行中调整 */
  mode: "enable" | "adjust";
  siteName: string;
  site: Site;
  /** 当前刷流配置（新契约 Site 视图不携带，从刷流统计接口读） */
  boost?: SiteBoostStats;
  onClose: () => void;
  onChanged: (site: Site) => void;
}) {
  const confirm = useConfirm();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [budgetGib, setBudgetGib] = useState("");
  const [holdDays, setHoldDays] = useState("");

  useEffect(() => {
    if (!open) return;
    setError(null);
    setBudgetGib(String(Math.round((boost?.boost.budget_bytes ?? 0) / GIB)));
    setHoldDays(String(boost?.boost.hold_days ?? 0));
  }, [open, boost?.boost.budget_bytes, boost?.boost.hold_days]);

  async function save() {
    const gib = Math.round(Number(budgetGib.trim()));
    if (!Number.isFinite(gib) || gib < 1) {
      setError("刷流预算必须是不小于 1 的整数（单位 GiB）");
      return;
    }
    const days = Math.round(Number(holdDays.trim()));
    if (!Number.isFinite(days) || days < 0 || days > 30) {
      setError("汰换保留期须是 0～30 之间的整数（天）");
      return;
    }
    const currentGib = Math.round((boost?.boost.budget_bytes ?? 0) / GIB);
    if (mode === "adjust" && gib < currentGib) {
      const ok = await confirm({
        title: `将「${siteName}」的刷流预算从 ${currentGib} GiB 调小到 ${gib} GiB？`,
        bullets: [
          "在池占用超出新预算的部分将被汰换：连同已下载的数据一起删除，且同一种子不会再抢回",
          "按上传效率从低到高删——死种和低效的先走，高效种子最后才会被动",
          "汰换保留期内的任务不受影响，到期后才继续收敛",
          "收敛期间暂停接新的免费种",
        ],
        confirmLabel: "确认调小",
        tone: "danger",
      });
      if (!ok) return;
    }
    setBusy(true);
    setError(null);
    try {
      onChanged(await setSiteRatioBoost(site.id, true, gib * GIB, days));
      onClose();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      open={open}
      onClose={onClose}
      label={mode === "enable" ? "开启自动刷分享率" : "刷流设置"}
      width="lg"
      topmost
    >
      <div className="space-y-4 p-6">
        <div>
          <h2 className="text-title font-bold text-[var(--text)]">
            {mode === "enable" ? `开启「${siteName}」的自动刷分享率？` : `刷流设置 · ${siteName}`}
          </h2>
          <p className="mt-1 text-sub leading-6 text-[var(--text-muted)]">
            {mode === "enable"
              ? "开启后将自动抢该站新发布的免费种子做种以提升分享率，占用空间在预算内自动汰换" +
                "（下载完成、入池满保留期且上传效率过低的任务才会被连数据删除），" +
                "该站的索引同步会提速到约 5 分钟一次。"
              : "调小预算会按上传效率从低到高汰换在池任务（连数据删除），直到占用回到新预算内；" +
                "保留期内的任务绝不会被提前删除。"}
          </p>
        </div>

        {error && (
          <div className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-4 py-3 text-body text-[#ff6b6b]">
            {error}
          </div>
        )}

        <div className="grid grid-cols-2 gap-3 max-md:grid-cols-1">
          <BoostField
            label="存储预算"
            value={budgetGib}
            unit="GiB"
            min={1}
            disabled={busy}
            onChange={setBudgetGib}
            hint="刷流任务占用磁盘的上限，预算内自动汰换"
          />
          <BoostField
            label="汰换保留期"
            value={holdDays}
            unit="天"
            min={0}
            disabled={busy}
            onChange={setHoldDays}
            hint="H&R 安全垫：有考核的站不小于考核时长；无考核可调 0 自由汰换"
          />
        </div>

        <div className="flex justify-end gap-2.5 pt-1">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="btn-glass px-4 py-2 text-sub font-medium"
          >
            取消
          </button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={busy}
            className="btn-accent rounded-full px-4 py-2 text-sub font-semibold disabled:opacity-60"
          >
            {busy ? "保存中…" : mode === "enable" ? "开启刷流" : "保存"}
          </button>
        </div>
      </div>
    </Modal>
  );
}

/** 带单位后缀与说明的数字输入（弹窗内两枚字段共用）。 */
function BoostField({
  label,
  value,
  unit,
  min,
  disabled,
  onChange,
  hint,
}: {
  label: string;
  value: string;
  unit: string;
  min: number;
  disabled?: boolean;
  onChange: (value: string) => void;
  hint: string;
}) {
  return (
    <div>
      <label className="mb-1.5 block text-caption font-medium text-[var(--text-muted)]">
        {label}
      </label>
      <div className="relative">
        <input
          type="number"
          min={min}
          value={value}
          disabled={disabled}
          onChange={(e) => onChange(e.target.value)}
          className="w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 pr-12 text-ui text-white outline-none transition [appearance:textfield] focus:border-white/25 [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none"
        />
        <span className="pointer-events-none absolute inset-y-0 right-3 flex items-center text-caption font-medium text-[var(--text-faint)]">
          {unit}
        </span>
      </div>
      <p className="mt-1 text-caption leading-4 text-[var(--text-faint)]">{hint}</p>
    </div>
  );
}

/** 刷流行级读数：只回答「刷流在运行吗 + 预算多少」。 */
export function BoostReadout({ boost }: { boost?: SiteBoostStats }) {
  const budget = boost?.boost.budget_bytes ?? 0;
  return (
    <div
      className="flex min-w-0 items-center gap-1.5"
      title="自动刷分享率运行中（完整设置见 ⋯ 菜单的「刷流设置」）"
    >
      <span className="shrink-0 text-caption font-medium text-[var(--accent)]">刷流</span>
      <span className="truncate text-caption text-[var(--text-muted)]">
        {budget > 0 ? `预算 ${formatBytes(budget)}` : "运行中"}
      </span>
    </div>
  );
}
