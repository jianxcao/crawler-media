"use client";

import { useEffect, useState } from "react";

import { useConfirm, useToast } from "@/components/feedback";
import {
  deleteRuleSet,
  listRuleSets,
  setDefaultRuleSet,
  type RuleSet,
  type RuleSetAtom,
} from "@/lib/api/subscriptions";
import { ruleSetSummary } from "@/lib/rule-set-summary";

import { RuleSetEditorDialog } from "./rule-sets/rule-set-editor-dialog";

/**
 * 规则组管理（设置 → 订阅规则 → 规则组）：Web 端唯一的规则组配置入口。
 *
 * 设计原则：
 * - **表单化，不让用户手写 JSON**——原子的每个维度铺成对应控件；
 * - **列表顺序即偏好**：分辨率按点击顺序入列（与后端评分语义一致），
 *   选中的芯片上标序号，先选的优先；
 * - **摘要芯片**：清单里把原子翻译成人话（"2160p > 1080p · 仅免费"），
 *   看一眼就知道每个组在过滤什么；
 * - **复制微调**：订阅不做 per-订阅 override，"想微调就复制一个规则组"是
 *   既定产品哲学——复制按钮预填原组条件，改名保存即成新组；
 * - **删除保护前置**：默认组与被订阅引用的组直接禁用删除按钮并说明原因，
 *   不让用户点了再吃后端 409。
 *
 * ruleSetSummary 与 RuleSetEditorDialog 导出复用：订阅弹窗（选组时看清内容、
 * 快捷新建）与订阅详情页（换组）共用同一套摘要与编辑器。
 */

export { ruleSetSummary, upgradeTargetLabel } from "@/lib/rule-set-summary";
export { RuleSetEditorDialog } from "./rule-sets/rule-set-editor-dialog";

/** 编辑器打开参数：ruleSet 有值=编辑；null=新建，template 提供预填（复制场景）。 */
interface EditorTarget {
  ruleSet: RuleSet | null;
  template: { name: string; atoms: RuleSetAtom[]; keepOldVersions?: boolean } | null;
}

export function RuleSetsPanel() {
  const confirm = useConfirm();
  const toast = useToast();
  const [ruleSets, setRuleSets] = useState<RuleSet[] | null>(null);
  const [editing, setEditing] = useState<EditorTarget | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = () => {
    void listRuleSets()
      .then((rows) => {
        setRuleSets(rows);
        setError(null);
      })
      .catch(() => setRuleSets([]));
  };

  useEffect(reload, []);

  const makeDefault = async (rs: RuleSet) => {
    try {
      await setDefaultRuleSet(rs.id);
      toast.success(`「${rs.name}」已设为默认规则组`);
      reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : "设置失败，请稍后重试");
    }
  };

  const remove = async (rs: RuleSet) => {
    if (
      !(await confirm({
        title: `删除规则组「${rs.name}」？`,
        confirmLabel: "删除",
        tone: "danger",
      }))
    ) {
      return;
    }
    try {
      await deleteRuleSet(rs.id);
      reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : "删除失败，请稍后重试");
    }
  };

  return (
    <section>
      <div className="mb-2 flex items-center justify-between">
        <h3 className="text-body font-semibold text-white/90">规则组</h3>
        <button
          type="button"
          onClick={() => setEditing({ ruleSet: null, template: null })}
          className="btn-glass px-3 py-1.5 text-sub font-medium"
        >
          + 新建规则组
        </button>
      </div>
      <p className="mb-4 text-sub leading-6 text-[var(--text-muted)]">
        规则组定义「什么样的资源可接受」——硬性条件（分辨率、编码、体积、免费等）
        与偏好顺序，在订阅弹窗中按订阅选用；标「默认」的组是新订阅的初始选择。
        修改只影响之后的资源评估，已下载的内容不受影响。
      </p>

      {error && (
        <p className="mb-3 rounded-lg border border-red-400/25 bg-red-500/10 px-3.5 py-2.5 text-sub leading-6 text-red-200">
          {error}
        </p>
      )}

      {ruleSets === null ? (
        <p className="rounded-xl bg-white/[0.03] px-4 py-4 text-ui text-[var(--text-muted)]">
          正在加载…
        </p>
      ) : (
        <div className="space-y-1.5">
          {ruleSets.map((rs) => {
            const chips = ruleSetSummary(rs.atoms, { keepOldVersions: rs.keep_old_versions });
            // 新契约不再下发被引用数：删除保护只剩「默认组不可删」
            const deleteBlock = rs.is_default ? "默认规则组不可删除" : null;
            return (
              <div
                key={rs.id}
                className="flex items-center gap-3 rounded-xl border border-white/[0.08] bg-white/[0.04] px-4 py-2.5"
              >
                <div className="min-w-0 flex-1">
                  <p className="flex items-center gap-2">
                    <span className="truncate text-ui font-medium text-white/90">{rs.name}</span>
                    {rs.is_default && (
                      <span className="shrink-0 rounded-full border border-white/[0.14] bg-white/[0.1] px-2 py-0.5 text-micro font-semibold text-white/80">
                        默认
                      </span>
                    )}
                  </p>
                  <p className="mt-1 flex flex-wrap gap-1.5">
                    {chips.length === 0 ? (
                      <span className="text-caption text-[var(--text-faint)]">
                        全不限：任何识别为本条目的资源都可接受
                      </span>
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
                  </p>
                </div>
                {!rs.is_default && (
                  <button
                    type="button"
                    title="新订阅未指定规则组时使用本组（不改已有订阅）"
                    onClick={() => void makeDefault(rs)}
                    className="btn-glass shrink-0 px-3 py-1.5 text-sub font-medium"
                  >
                    设为默认
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => setEditing({ ruleSet: rs, template: null })}
                  className="btn-glass shrink-0 px-3 py-1.5 text-sub font-medium"
                >
                  编辑
                </button>
                <button
                  type="button"
                  title="以本组条件为底新建一个规则组"
                  onClick={() =>
                    setEditing({
                      ruleSet: null,
                      template: {
                        name: `${rs.name} 副本`,
                        atoms: rs.atoms,
                        keepOldVersions: rs.keep_old_versions,
                      },
                    })
                  }
                  className="btn-glass shrink-0 px-3 py-1.5 text-sub font-medium"
                >
                  复制
                </button>
                <button
                  type="button"
                  disabled={deleteBlock !== null}
                  title={deleteBlock ?? undefined}
                  onClick={() => void remove(rs)}
                  className="shrink-0 rounded-full border border-red-400/25 bg-red-500/[0.08] px-3 py-1.5 text-sub font-medium text-red-200/90 transition hover:bg-red-500/20 disabled:cursor-not-allowed disabled:opacity-35"
                >
                  删除
                </button>
              </div>
            );
          })}
        </div>
      )}

      {editing !== null && (
        <RuleSetEditorDialog
          ruleSet={editing.ruleSet}
          template={editing.template ?? undefined}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            reload();
          }}
        />
      )}
    </section>
  );
}
