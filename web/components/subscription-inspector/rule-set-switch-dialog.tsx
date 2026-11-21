"use client";

import { useState } from "react";

import { Modal } from "@/components/modal";
import { ruleSetSummary } from "@/components/rule-sets-panel";
import type { RuleSet } from "@/lib/api/subscriptions";

/**
 * 换规则组弹窗：列出全部规则组（含条件摘要），点选即应用。
 * 只影响之后的资源评估——已投递/已入库的工单不追溯。
 */
export function RuleSetSwitchDialog({
  ruleSets,
  currentId,
  onClose,
  onPick,
}: {
  ruleSets: RuleSet[];
  currentId: string | null;
  onClose: () => void;
  onPick: (ruleSetId: string) => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pick = async (id: string) => {
    if (id === currentId) {
      onClose();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onPick(id);
    } catch (e) {
      setError(e instanceof Error ? e.message : "更换失败，请稍后重试");
      setBusy(false);
    }
  };

  return (
    <Modal open onClose={onClose} label="更换规则组" width="lg">
      <div className="scroll-thin max-h-[76dvh] overflow-y-auto p-6 max-md:p-5">
        <h2 className="text-title font-bold text-white">更换规则组</h2>
        <p className="mt-1 text-sub leading-6 text-[var(--text-muted)]">
          点选即应用，只影响之后的资源评估；已下载/已入库的内容不受影响。
          需要新的组合条件可去「设置 → 订阅规则 → 规则组」新建。
        </p>
        {error && (
          <p className="mt-3 rounded-lg border border-red-400/25 bg-red-500/10 px-3.5 py-2.5 text-sub leading-6 text-red-200">
            {error}
          </p>
        )}
        <div className="mt-4 space-y-1.5">
          {ruleSets.map((rs) => {
            const chips = ruleSetSummary(rs.atoms, { keepOldVersions: rs.keep_old_versions });
            const current = rs.id === currentId;
            return (
              <button
                key={rs.id}
                type="button"
                disabled={busy}
                onClick={() => void pick(rs.id)}
                className={`w-full rounded-xl border px-4 py-2.5 text-left transition disabled:opacity-50 ${
                  current
                    ? "border-white/25 bg-white/[0.1]"
                    : "border-white/[0.08] bg-white/[0.03] hover:bg-white/[0.07]"
                }`}
              >
                <span className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate text-ui font-medium text-white/90">
                    {rs.name}
                    {rs.is_default && (
                      <span className="ml-2 text-caption font-normal text-[var(--text-faint)]">
                        默认
                      </span>
                    )}
                  </span>
                  {current && (
                    <span className="shrink-0 text-caption font-medium text-[var(--ok)]">
                      当前使用 ✓
                    </span>
                  )}
                </span>
                <span className="mt-1 flex flex-wrap gap-1.5">
                  {chips.length === 0 ? (
                    <span className="text-caption text-[var(--text-faint)]">全不限</span>
                  ) : (
                    chips.map((chip) => (
                      <span
                        key={chip}
                        className="rounded-md bg-white/[0.07] px-1.5 py-0.5 text-caption text-white/75"
                      >
                        {chip}
                      </span>
                    ))
                  )}
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </Modal>
  );
}
