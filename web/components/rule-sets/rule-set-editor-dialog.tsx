"use client";

import { useEffect, useMemo, useState } from "react";

import { Modal } from "@/components/modal";
import { listConfiguredSites, type Site } from "@/lib/api/sites";
import {
  createRuleSet,
  type RuleSet,
  type RuleSetAtom,
  updateRuleSet,
} from "@/lib/api/subscriptions";
import { PLATFORM_OPTIONS, platformLabel } from "@/lib/platforms";
import { ruleAtomValue, ruleAtomValues, ruleSetEditAtoms } from "@/lib/rule-set-edit";
import {
  buildRuleSetDraft,
  RESOLUTION_OPTIONS,
  ruleSetSummary,
} from "@/lib/rule-set-summary";
import {
  CODEC_FAMILIES,
  DEFAULT_LADDER,
  LADDER_OPTIONS,
  MEDIA_SOURCE_OPTIONS,
  upgradeLadderPreview,
} from "@/lib/upgrade-ladder";

import {
  DownloadLimitsSection,
  PreferredSitesField,
  QualitySourceSection,
} from "./rule-set-editor-sections";
import { Field, ToggleChip } from "./rule-set-form-primitives";
import { RuleSetUpgradeField } from "./rule-set-upgrade-field";
import { UpgradeLadderPreviewView } from "./upgrade-ladder-preview";

// ---------------------------------------------------------------------------
// 编辑器弹窗
// ---------------------------------------------------------------------------

export function RuleSetEditorDialog({
  ruleSet,
  template,
  raised = false,
  onClose,
  onSaved,
}: {
  /** null = 新建 */
  ruleSet: RuleSet | null;
  /** 新建时的预填内容（复制场景）；编辑时忽略 */
  template?: { name: string; atoms: RuleSetAtom[]; keepOldVersions?: boolean };
  /** 叠在其他弹窗之上时置 true（如订阅弹窗里快捷新建） */
  raised?: boolean;
  onClose: () => void;
  /** 保存成功回调，参数是后端返回的最新规则组（快捷新建场景要选中它） */
  onSaved: (saved: RuleSet) => void;
}) {
  const atoms = useMemo(() => ruleSet?.atoms ?? template?.atoms ?? [], [ruleSet, template]);
  const keepOldVersions = ruleSet?.keep_old_versions ?? template?.keepOldVersions ?? false;

  const [name, setName] = useState(ruleSet?.name ?? template?.name ?? "");
  const [resolutions, setResolutions] = useState<string[]>(ruleAtomValues(atoms, "resolution"));
  const [codecFamilies, setCodecFamilies] = useState<Set<string>>(
    () =>
      new Set(
        CODEC_FAMILIES.filter((f) =>
          f.values.some((v) => ruleAtomValues(atoms, "video_codec").includes(v)),
        ).map((f) => f.label),
      ),
  );
  // 平台白/黑名单：界面只维护"常驻选项"部分，其余值走 platformExtras 透传
  const [platformsAllow, setPlatformsAllow] = useState<string[]>(() =>
    ruleAtomValues(atoms, "platform").filter((v) => PLATFORM_OPTIONS.includes(v)),
  );
  const [platformsBlock, setPlatformsBlock] = useState<string[]>(() =>
    ruleAtomValues(atoms, "platform", true).filter((v) => PLATFORM_OPTIONS.includes(v)),
  );
  const [mediaSources, setMediaSources] = useState<string[]>(ruleAtomValues(atoms, "source"));
  const [configuredSites, setConfiguredSites] = useState<Site[]>([]);
  const [preferredSites, setPreferredSites] = useState<string[]>(ruleAtomValues(atoms, "site"));
  useEffect(() => {
    listConfiguredSites().then(setConfiguredSites).catch(() => setConfiguredSites([]));
  }, []);

  const [upgradeLadder, setUpgradeLadder] = useState<string[]>(
    () => ruleAtomValue(atoms, "upgrade_ladder")?.split(",") ?? [...DEFAULT_LADDER],
  );
  const [hdrLevels, setHdrLevels] = useState<string[]>(() => ruleAtomValues(atoms, "hdr"));
  const [hdrBlock, setHdrBlock] = useState<string[]>(() => ruleAtomValues(atoms, "hdr", true));
  const [subLangs, setSubLangs] = useState<string[]>(
    ruleAtomValues(atoms, "subtitle_language"),
  );
  const [audioLangs, setAudioLangs] = useState<string[]>(
    ruleAtomValues(atoms, "audio_language"),
  );
  const [freeOnly, setFreeOnly] = useState(atoms.some((atom) => atom.kind === "free" && !atom.exclude));
  const [excludeHr, setExcludeHr] = useState(atoms.some((atom) => atom.kind === "hr" && atom.exclude));
  const [hrStrict, setHrStrict] = useState(
    atoms.some((atom) => atom.kind === "hr" && atom.exclude && atom.unknown === "exclude"),
  );
  const [minSeeders, setMinSeeders] = useState(ruleAtomValue(atoms, "min_seeders")?.toString() ?? "");
  const [sizeMin, setSizeMin] = useState(ruleAtomValue(atoms, "size")?.split("-")[0]?.toString() ?? "");
  const [sizeMax, setSizeMax] = useState(ruleAtomValue(atoms, "size")?.split("-")[1]?.toString() ?? "");
  const [groupsAllow, setGroupsAllow] = useState(ruleAtomValues(atoms, "team").join(", "));
  const [groupsBlock, setGroupsBlock] = useState(ruleAtomValues(atoms, "team", true).join(", "));
  // 洗版目标：""=不洗版
  const [upgradeSource, setUpgradeSource] = useState<string>(
    ruleAtomValue(atoms, "wash_target")?.split(" ")[1] ?? "",
  );
  const [upgradeKeepOld, setUpgradeKeepOld] = useState<boolean>(keepOldVersions);
  const [cutoffResolution, setCutoffResolution] = useState<string>(
    ruleAtomValue(atoms, "wash_target")?.split(" ")[0] ?? "",
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 编码族之外的原有值（手工经 API 写入的自定义编码）：编辑时不丢
  const codecExtras = useMemo(
    () =>
      ruleAtomValues(atoms, "video_codec").filter(
        (v) => !CODEC_FAMILIES.some((f) => f.values.includes(v)),
      ),
    [atoms],
  );

  // 常驻选项之外的既有平台值（经 API 配置的小众平台）：编辑时不丢
  const platformExtras = useMemo(
    () => ({
      allow: ruleAtomValues(atoms, "platform").filter((v) => !PLATFORM_OPTIONS.includes(v)),
      block: ruleAtomValues(atoms, "platform", true).filter((v) => !PLATFORM_OPTIONS.includes(v)),
    }),
    [atoms],
  );

  /**
   * 三态循环：不限 → 只要 → 排除 → 不限。同一平台因此不可能同时进白/黑名单
   * （后端读取路径有意保持宽容、不对矛盾配置报错，矛盾在这里从源头消除）。
   */
  const cyclePlatform = (value: string) => {
    if (!platformsAllow.includes(value) && !platformsBlock.includes(value)) {
      setPlatformsAllow((prev) => [...prev, value]);
    } else if (platformsAllow.includes(value)) {
      setPlatformsAllow((prev) => prev.filter((v) => v !== value));
      setPlatformsBlock((prev) => [...prev, value]);
    } else {
      setPlatformsBlock((prev) => prev.filter((v) => v !== value));
    }
  };

  const cycleHdr = (value: string) => {
    if (!hdrLevels.includes(value) && !hdrBlock.includes(value)) {
      setHdrLevels((prev) => [...prev, value]);
    } else if (hdrLevels.includes(value)) {
      setHdrLevels((prev) => prev.filter((v) => v !== value));
      setHdrBlock((prev) => [...prev, value]);
    } else {
      setHdrBlock((prev) => prev.filter((v) => v !== value));
    }
  };

  /**
   * 阶梯维度：交互与分辨率偏好完全一致——点击依次入列，序号即位次。
   * 最后一维不允许移除：空阶梯没有任何一位可比，后端会回落到缺省二元组，
   * 界面上却显示"一个都没选"——不让用户进入这种口径不一致的状态。
   */
  const toggleLadderDim = (value: string) =>
    setUpgradeLadder((prev) => {
      if (!prev.includes(value)) return [...prev, value];
      return prev.length > 1 ? prev.filter((v) => v !== value) : prev;
    });

  /** 某维度是否还没配偏好——后端会把这种位直接跳过。 */
  const ladderUnconfigured = (dim: string) =>
    (dim === "hdr" && !hdrLevels.length) ||
    (dim === "video_codec" && !codecFamilies.size && !codecExtras.length) ||
    (dim === "platform" && !platformsAllow.length && !platformExtras.allow.length);

  // 名称维度上浮的告警条件（§14.4）：平台识别率最低，排在前面会截断整条比较。
  // 按**生效**维度判断——被跳过的维度不占位，否则"平台后面跟着一个未配置的
  // 编码位"会误报
  const effectiveLadder = upgradeLadder.filter((dim) => !ladderUnconfigured(dim));
  const ladderText = effectiveLadder
    .map((dim) => LADDER_OPTIONS.find((o) => o.value === dim)?.label ?? dim)
    .join(" › ");
  const platformIndex = effectiveLadder.indexOf("platform");
  const platformNotLast =
    platformIndex >= 0 && platformIndex < effectiveLadder.length - 1;

  const toggleResolution = (value: string) =>
    setResolutions((prev) =>
      prev.includes(value) ? prev.filter((r) => r !== value) : [...prev, value],
    );

  /**
   * 片源档：交互与分辨率偏好完全一致（点击依次入列，序号即位次）。
   * 收窄白名单时若洗版终点被排除在外，直接把终点清掉——不让用户进入
   * "保存时才报错"的状态（与平台三态那处"矛盾在源头消除"同一取向）。
   */
  const toggleMediaSource = (value: string) =>
    setMediaSources((prev) => {
      const next = prev.includes(value)
        ? prev.filter((v) => v !== value)
        : [...prev, value];
      if (upgradeSource && next.length && !next.includes(upgradeSource)) {
        setUpgradeSource("");
      }
      return next;
    });

  const toggleCodecFamily = (label: string) =>
    setCodecFamilies((prev) => {
      const next = new Set(prev);
      if (next.has(label)) next.delete(label);
      else next.add(label);
      return next;
    });

  const draft = useMemo(
    () =>
      buildRuleSetDraft({
        resolutions,
        mediaSources,
        codecFamilies,
        codecExtras,
        platformsAllow,
        platformsBlock,
        platformExtras,
        groupsAllow,
        groupsBlock,
        hdrLevels,
        hdrBlock,
        subLangs,
        audioLangs,
        preferredSites,
        freeOnly,
        excludeHr,
        hrStrict,
        minSeeders,
        sizeMin,
        sizeMax,
        upgradeSource,
        cutoffResolution,
        upgradeLadder,
      }),
    [
      resolutions,
      mediaSources,
      codecFamilies,
      codecExtras,
      platformsAllow,
      platformsBlock,
      platformExtras,
      groupsAllow,
      groupsBlock,
      hdrLevels,
      hdrBlock,
      subLangs,
      audioLangs,
      preferredSites,
      freeOnly,
      excludeHr,
      hrStrict,
      minSeeders,
      sizeMin,
      sizeMax,
      upgradeSource,
      cutoffResolution,
      upgradeLadder,
    ],
  );

  // Compare against the initial editor projection, including codec-family and
  // default-control normalization, rather than reconstructing the stored atoms.
  const [initialDraftAtoms] = useState(() => draft.atoms);

  // 底部回执分两段：硬性筛选走芯片，洗版走阶梯（洗版开着时芯片里不再重复目标）
  const ladderPreview = useMemo(
    () => upgradeLadderPreview(draft.atoms, platformLabel),
    [draft.atoms],
  );
  const draftChips = useMemo(
    () =>
      ruleSetSummary(draft.atoms, {
        withoutUpgrade: ladderPreview !== null,
        keepOldVersions: upgradeKeepOld,
      }),
    [draft.atoms, ladderPreview, upgradeKeepOld],
  );

  const submit = async () => {
    if (draft.error) {
      setError(draft.error);
      return;
    }
    const next = ruleSetEditAtoms(atoms, initialDraftAtoms, draft.atoms);
    setBusy(true);
    setError(null);
    try {
      let saved: RuleSet;
      if (ruleSet === null) {
        saved = await createRuleSet(name.trim(), next, upgradeKeepOld);
      } else {
        // 未修改的维度保留原始 priority/exclude；实际修改的值必须进入请求。
        saved = await updateRuleSet(ruleSet.id, name.trim(), next, upgradeKeepOld);
      }
      onSaved(saved);
    } catch (e) {
      setError(e instanceof Error ? e.message : "保存失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open
      onClose={onClose}
      label={ruleSet ? `编辑规则组「${ruleSet.name}」` : "新建规则组"}
      width="lg"
      raised={raised}
      panelClassName="max-h-[76dvh]"
    >
      {/* 头部常驻：规则组表单比一屏长得多，标题要一直在 */}
      <div className="border-b border-white/[0.07] px-6 pb-4 pt-6 max-md:px-5">
        <h2 className="text-title font-bold text-white">
          {ruleSet ? "编辑规则组" : "新建规则组"}
        </h2>
        <p className="mt-1 text-sub text-[var(--text-muted)]">
          所有条件都可以留空 = 不限该维度；条件之间是「且」的关系。
        </p>
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto p-6 max-md:p-5">
        <div className="space-y-5">
          <Field label="名称">
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="如：4K 免费、追剧省流"
              className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
            />
          </Field>

          <Field
            label="分辨率"
            hint="点击依次选择，先选的优先（选中顺序 = 下载偏好）；不选 = 不限。限定分辨率后，无法从种子名识别出分辨率的资源也会被排除，可用「手动选种」兜底"
          >
            <div className="flex flex-wrap gap-1.5">
              {RESOLUTION_OPTIONS.map((option) => {
                const index = resolutions.indexOf(option);
                return (
                  <ToggleChip
                    key={option}
                    active={index >= 0}
                    onClick={() => toggleResolution(option)}
                  >
                    {index >= 0 && resolutions.length > 1 && (
                      <span className="mr-1.5 inline-flex size-4 items-center justify-center rounded-full bg-white/20 text-micro font-semibold">
                        {index + 1}
                      </span>
                    )}
                    {option}
                  </ToggleChip>
                );
              })}
            </div>
          </Field>

          {/* 片源与分辨率是用户谈论"版本好坏"时实际用的两个主轴（§2.2），也是
              仅有的两个偏好序参与候选选优的维度，因此并排放在最外层；它还必须
              排在洗版之前——洗版终点档只能从这个白名单里挑 */}
          <PreferredSitesField
            preferredSites={preferredSites}
            configuredSites={configuredSites}
            onPreferredSites={setPreferredSites}
          />

          <Field
            label="片源"
            hint="点击依次选择，先选的优先（选中顺序 = 下载偏好）；不选 = 不限。Rip 类 = WEBRip/BDRip，电视录制类 = HDTV/DVD。限定片源后，无法从种子名识别出片源的资源也会被排除，可用「手动选种」兜底"
          >
            <div className="flex flex-wrap gap-1.5">
              {MEDIA_SOURCE_OPTIONS.map((option) => {
                const index = mediaSources.indexOf(option.value);
                return (
                  <ToggleChip
                    key={option.value}
                    active={index >= 0}
                    onClick={() => toggleMediaSource(option.value)}
                  >
                    {index >= 0 && mediaSources.length > 1 && (
                      <span className="mr-1.5 inline-flex size-4 items-center justify-center rounded-full bg-white/20 text-micro font-semibold">
                        {index + 1}
                      </span>
                    )}
                    {option.label}
                  </ToggleChip>
                );
              })}
            </div>
          </Field>

          <QualitySourceSection
            codecFamilies={codecFamilies}
            codecExtras={codecExtras}
            onToggleCodecFamily={toggleCodecFamily}
            platformsAllow={platformsAllow}
            platformsBlock={platformsBlock}
            platformExtras={platformExtras}
            onCyclePlatform={cyclePlatform}
            hdrLevels={hdrLevels}
            hdrBlock={hdrBlock}
            onCycleHdr={cycleHdr}
            groupsAllow={groupsAllow}
            onGroupsAllow={setGroupsAllow}
            groupsBlock={groupsBlock}
            onGroupsBlock={setGroupsBlock}
          />

          <DownloadLimitsSection
            subLangs={subLangs}
            onSubLangs={setSubLangs}
            audioLangs={audioLangs}
            onAudioLangs={setAudioLangs}
            freeOnly={freeOnly}
            onFreeOnly={setFreeOnly}
            excludeHr={excludeHr}
            onExcludeHr={setExcludeHr}
            hrStrict={hrStrict}
            onHrStrict={setHrStrict}
            minSeeders={minSeeders}
            onMinSeeders={setMinSeeders}
            sizeMin={sizeMin}
            onSizeMin={setSizeMin}
            sizeMax={sizeMax}
            onSizeMax={setSizeMax}
          />

          <RuleSetUpgradeField
            resolutions={resolutions}
            mediaSources={mediaSources}
            upgradeSource={upgradeSource}
            onUpgradeSource={setUpgradeSource}
            cutoffResolution={cutoffResolution}
            onCutoffResolution={setCutoffResolution}
            upgradeKeepOld={upgradeKeepOld}
            onUpgradeKeepOld={setUpgradeKeepOld}
            upgradeLadder={upgradeLadder}
            onToggleLadderDim={toggleLadderDim}
            ladderText={ladderText}
            ladderUnconfigured={ladderUnconfigured}
            platformNotLast={platformNotLast}
          />

          {/* 配完给一句人话回执：新用户最缺的就是"我配的到底是什么"的确认。
              内容来自与保存同一个 draft，不会出现"看到的和存下去的不一样" */}
          <div className="rounded-xl bg-white/[0.03] px-4 py-3">
            <p className="text-caption text-[var(--text-faint)]">
              {ladderPreview ? "先这样筛掉不要的" : "这条规则会这样筛选"}
            </p>
            <p className="mt-1 text-sub leading-6 text-[var(--text-muted)]">
              {draftChips.length
                ? draftChips.join(" · ")
                : "不限任何条件——身份对得上的资源都接受"}
            </p>
            {ladderPreview && (
              <UpgradeLadderPreviewView preview={ladderPreview} keepOld={upgradeKeepOld} />
            )}
          </div>

          {error && (
            <p className="rounded-lg border border-red-400/25 bg-red-500/10 px-3.5 py-2.5 text-sub leading-6 text-red-200">
              {error}
            </p>
          )}
        </div>
      </div>

      {/* 底栏常驻 */}
      <div className="flex justify-end gap-3 border-t border-white/[0.07] px-6 py-4 max-md:px-5">
        <button type="button" onClick={onClose} className="btn-glass h-9 px-4 text-ui font-medium">
          取消
        </button>
        <button
          type="button"
          disabled={busy || !name.trim()}
          onClick={() => void submit()}
          className="btn-accent h-9 rounded-full px-5 text-ui font-semibold disabled:opacity-50"
        >
          {busy ? "正在保存…" : "保存"}
        </button>
      </div>
    </Modal>
  );
}
