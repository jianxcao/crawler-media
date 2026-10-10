"use client";

import { useCallback, useEffect, useMemo, useState } from "react";

import { BrandLoader } from "@/components/brand-loader";
import { useToast } from "@/components/feedback";
import { CheckIcon, PlusIcon, XIcon } from "@/components/icons";
import {
  SCRAPE_DEFAULTS,
  fetchCountryCandidates,
  fetchLanguageCandidates,
  getScrapeConfig,
  previewScrapeNaming,
  saveScrapeConfig,
  type NamingPreview,
  type ScrapeConfig,
  type ScrapeSetting,
} from "@/lib/api/scrape";

/**
 * 刮削与整理（metadata.scrape）：语言与分级、图片选图、命名模板、目录写入。
 * 空字段 = 跟随默认；保存即生效，存量需整库刷新生效（提示文案）。
 */

type TabId = "metadata" | "images" | "naming" | "mirror" | "markers";

const TABS: { id: TabId; label: string; hint: string }[] = [
  { id: "metadata", label: "元数据", hint: "语言优先级与分级地区" },
  { id: "images", label: "图片", hint: "海报 / 背景图的选图口味与质量档位" },
  { id: "naming", label: "命名与整理", hint: "入库文件名模板" },
  { id: "mirror", label: "目录写入", hint: "侧车文件（NFO / 图片）写不写" },
  { id: "markers", label: "片头片尾", hint: "TheIntroDB 云端片头片尾标记库与自动跳过" },
];

const LANGUAGE_OPTIONS = [
  ["zh-CN", "简体中文"],
  ["en-US", "English"],
  ["ja-JP", "日本語"],
  ["ko-KR", "한국어"],
  ["zh-TW", "繁體中文"],
  ["fr-FR", "Français"],
  ["de-DE", "Deutsch"],
  ["ru-RU", "Русский"],
] as const;

const IMAGE_LANGUAGE_OPTIONS: { value: string; label: string; hint?: string }[] = [
  { value: "meta", label: "跟随元数据主语言", hint: "引用「元数据」语言优先级第 1 位" },
  { value: "", label: "无文字", hint: "语言为 null 的候选（无烧录文字）" },
  { value: "en", label: "English" },
  { value: "zh", label: "中文" },
  { value: "ja", label: "日本語" },
  { value: "ko", label: "한국어" },
];

const REGION_OPTIONS = [
  ["CN", "中国大陆"],
  ["US", "美国"],
  ["HK", "香港"],
  ["TW", "台湾"],
  ["JP", "日本"],
  ["KR", "韩国"],
  ["GB", "英国"],
] as const;

const POSTER_SIZES = ["w342", "w500", "w780", "w1280", "original"];
const BACKDROP_SIZES = ["w780", "w1280", "original"];
const STILL_SIZES = ["w92", "w300", "w500", "original"];

const INPUT_CLASS =
  "w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui " +
  "text-[var(--text)] outline-none focus:border-[var(--accent)]/60";
const LABEL_CLASS = "mb-1.5 block text-sub font-medium text-[var(--text-muted)]";

export function ScrapeSettingsSection() {
  const toast = useToast();
  const [config, setConfig] = useState<ScrapeConfig | null>(null);
  const [draft, setDraft] = useState<ScrapeSetting>(SCRAPE_DEFAULTS);
  const [tab, setTab] = useState<TabId>("metadata");
  const [busy, setBusy] = useState(false);
  const [customLang, setCustomLang] = useState("");
  const [preview, setPreview] = useState<NamingPreview | null>(null);

  const reload = useCallback(() => {
    void getScrapeConfig()
      .then((next) => {
        setConfig(next);
        setDraft(next.setting);
      })
      .catch((error) => toast.error(`读取刮削设置失败：${(error as Error).message}`));
    void previewScrapeNaming()
      .then(setPreview)
      .catch(() => undefined);
  }, [toast]);

  useEffect(() => {
    reload();
  }, [reload]);

  const effective = config?.effective;
  const set = <K extends keyof ScrapeSetting>(key: K, value: ScrapeSetting[K]) =>
    setDraft((d) => ({ ...d, [key]: value }));

  const save = async () => {
    setBusy(true);
    try {
      const next = await saveScrapeConfig(draft);
      setConfig(next);
      setDraft(next.setting);
      void previewScrapeNaming().then(setPreview).catch(() => undefined);
      toast.success("刮削设置已保存；存量媒体需整库「刷新元数据」后生效");
    } catch (error) {
      toast.error(`保存失败：${(error as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  const dirty = useMemo(() => JSON.stringify(draft) !== JSON.stringify(config?.setting ?? SCRAPE_DEFAULTS), [config, draft]);

  if (!config) {
    return (
      <div className="flex items-center justify-center gap-2.5 py-16 text-ui text-[var(--text-muted)]">
        <BrandLoader className="size-5" />
        正在读取刮削设置…
      </div>
    );
  }

  return (
    <div>
      <div className="mb-5 flex flex-wrap gap-1.5">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            aria-pressed={tab === t.id}
            onClick={() => setTab(t.id)}
            title={t.hint}
            className={`rounded-full px-3.5 py-1.5 text-ui font-medium transition ${
              tab === t.id
                ? "bg-white/[0.14] text-white"
                : "text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-white"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      {tab === "metadata" && (
        <div className="grid grid-cols-1 gap-5 lg:grid-cols-2">
          <Group label="元数据语言优先级">
            <p className="mb-3 text-caption leading-5 text-white/45">
              首位 = 请求 TMDB 的语言；下一位在首位缺失时兜底。默认 {effective?.language_priority.join(" → ")}。
            </p>
            <PriorityEditor
              value={draft.language_priority}
              onChange={(v) => set("language_priority", v)}
              options={LANGUAGE_OPTIONS.map(([v, l]) => ({ value: v, label: l }))}
              placeholder="zh-CN"
              allowCustom
              searchable
              customValue={customLang}
              onCustomChange={setCustomLang}
            />
          </Group>
          <Group label="分级优先地区">
            <PriorityEditor
              value={draft.cert_country_priority}
              onChange={(v) => set("cert_country_priority", v)}
              options={REGION_OPTIONS.map(([v, l]) => ({ value: v, label: l }))}
              placeholder="CN"
              searchable="countries"
            />
          </Group>
        </div>
      )}

      {tab === "images" && (
        <div className="space-y-5">
          <Group label="海报选择">
            <select
              className={INPUT_CLASS}
              value={draft.poster_mode || "default"}
              onChange={(e) => set("poster_mode", e.target.value === "default" ? "" : e.target.value)}
            >
              <option value="default">TMDB 默认（与发现页一致）</option>
              <option value="language">按语言优先级挑选</option>
            </select>
            <p className="mt-2 text-caption leading-5 text-white/45">
              默认档位取 TMDB 官方海报；「按语言优先级」会逐级取第一档有候选的语言（订阅前后海报可能跳变）。
            </p>
          </Group>
          <Group label="海报语言优先级">
            <PriorityEditor
              value={draft.poster_language_priority}
              onChange={(v) => set("poster_language_priority", v)}
              options={IMAGE_LANGUAGE_OPTIONS}
              placeholder="meta"
            />
          </Group>
          <Group label="背景图语言优先级">
            <PriorityEditor
              value={draft.backdrop_language_priority}
              onChange={(v) => set("backdrop_language_priority", v)}
              options={IMAGE_LANGUAGE_OPTIONS}
              placeholder=""
            />
          </Group>
          <div className="grid grid-cols-2 gap-4 max-md:grid-cols-1">
            <Group label="海报最低宽度（门卡）">
              <NumberInput
                value={draft.poster_min_width}
                onChange={(v) => set("poster_min_width", v)}
                placeholder={String(effective?.poster_min_width ?? 500)}
              />
            </Group>
            <Group label="背景图最低宽度（门卡）">
              <NumberInput
                value={draft.backdrop_min_width}
                onChange={(v) => set("backdrop_min_width", v)}
                placeholder={String(effective?.backdrop_min_width ?? 1920)}
              />
            </Group>
          </div>
          <div className="grid grid-cols-3 gap-4 max-md:grid-cols-1">
            <Group label="海报档位">
              <SizeSelect value={draft.poster_size} onChange={(v) => set("poster_size", v)} options={POSTER_SIZES} placeholder={effective?.poster_size ?? "w780"} />
            </Group>
            <Group label="背景图档位">
              <SizeSelect value={draft.backdrop_size} onChange={(v) => set("backdrop_size", v)} options={BACKDROP_SIZES} placeholder={effective?.backdrop_size ?? "original"} />
            </Group>
            <Group label="剧照档位">
              <SizeSelect value={draft.still_size} onChange={(v) => set("still_size", v)} options={STILL_SIZES} placeholder={effective?.still_size ?? "w300"} />
            </Group>
          </div>
        </div>
      )}

      {tab === "naming" && (
        <div className="space-y-5">
          <Group label="命名模板（默认 = 现状）">
            <div className="space-y-4">
              <TemplateField label="条目目录" value={draft.naming_entry_dir} onChange={(v) => set("naming_entry_dir", v)} placeholder={effective?.naming_entry_dir} tokens={["{title}", "{original_title}", "{year}", "{tmdb_id}"]} />
              <TemplateField label="电影文件名" value={draft.naming_movie_file} onChange={(v) => set("naming_movie_file", v)} placeholder={effective?.naming_movie_file} tokens={["{title}", "{year}", "{resolution}", "{media_source}", "{release_group}", "{ext}"]} />
              <TemplateField label="季目录" value={draft.naming_season_dir} onChange={(v) => set("naming_season_dir", v)} placeholder={effective?.naming_season_dir} tokens={["{season}", "{season:02d}"]} />
              <TemplateField label="剧集文件名" value={draft.naming_episode_file} onChange={(v) => set("naming_episode_file", v)} placeholder={effective?.naming_episode_file} tokens={["{title}", "{season_episode}", "{episode_title}", "{season:02d}", "{episode:02d}", "{resolution}", "{ext}"]} />
            </div>
          </Group>
          {preview && (
            <Group label="实时预览（保存后的示例渲染）">
              <div className="space-y-1.5 font-mono text-caption text-white/60">
                <p>
                  <span className="mr-2 text-white/35">电影</span>
                  {preview.movie || "（空）"}
                </p>
                <p>
                  <span className="mr-2 text-white/35">剧集</span>
                  {preview.tv || "（空）"}
                </p>
              </div>
            </Group>
          )}
        </div>
      )}

      {tab === "mirror" && (
        <div className="space-y-5">
          <div className="grid grid-cols-1 gap-5 lg:grid-cols-2">
            <Group label="媒体目录写入">
              <ToggleRow
                label="写入图片（poster / fanart）"
                hint="自动选择的封面与背景图写进媒体目录（mirror_images）"
                value={draft.mirror_images}
                onChange={(v) => set("mirror_images", v)}
              />
              <ToggleRow
                label="写入 NFO"
                hint="标题 / 年份 / 别名写进 NFO（mirror_nfo）"
                value={draft.mirror_nfo}
                onChange={(v) => set("mirror_nfo", v)}
              />
              <ToggleRow
                label="分集剧照镜像"
                hint="单集剧照写入对应集目录（mirror_episode_thumbs）"
                value={draft.mirror_episode_thumbs}
                onChange={(v) => set("mirror_episode_thumbs", v)}
              />
            </Group>
            <Group label="Fanart.tv 扩展艺术图">
              <div className="space-y-2">
                <label className="text-sub font-medium text-white/80">Fanart.tv API Key</label>
                <input
                  type="password"
                  className={INPUT_CLASS}
                  placeholder="例如 fanart_xxxxxxxxxxxxxxxxxxxx"
                  value={draft.fanart_api_key ?? ""}
                  onChange={(e) => set("fanart_api_key", e.target.value)}
                />
                <p className="text-caption text-white/40">
                  电视剧用 TVDB id，电影用 TMDB id。留空则不下载 logo / thumb / banner / 季图。
                </p>
              </div>
            </Group>
          </div>
        </div>
      )}

      {tab === "markers" && (
        <div className="grid grid-cols-1 gap-5 lg:grid-cols-2">
          <Group label="TheIntroDB 云端片头片尾库">
            <ToggleRow
              label="启用 TheIntroDB"
              hint="优先通过社区开源数据库匹配高精度片头与片尾起止时间（无需本地下载与计算，对 STRM 极佳）"
              value={draft.theintrodb_enabled ?? false}
              onChange={(v) => set("theintrodb_enabled", v)}
            />
            <div className="mt-4 space-y-2">
              <label className="text-sub font-medium text-white/80">TheIntroDB API Key</label>
              <input
                type="password"
                className={INPUT_CLASS}
                placeholder="例如 tidb_xxxxxxxxxxxxxxxxxxxx"
                value={draft.theintrodb_api_key ?? ""}
                onChange={(e) => set("theintrodb_api_key", e.target.value)}
              />
              <p className="text-caption text-white/40">
                可前往{" "}
                <a
                  href="https://theintrodb.org/"
                  target="_blank"
                  rel="noreferrer"
                  className="text-amber-400 hover:underline"
                >
                  theintrodb.org
                </a>{" "}
                注册并获取免费 API Key。配置后系统在识别剧集时优先从云端拉取片头时间戳，自动向 Infuse 等客户端提供「跳过片头」能力。
              </p>
            </div>
          </Group>

          <Group label="本地声纹比对 (Chromaprint)">
            <div className="space-y-2">
              <label className="text-sub font-medium text-white/80">单集音频采样时长（秒）</label>
              <input
                type="number"
                min={30}
                max={3600}
                step={10}
                className={INPUT_CLASS}
                placeholder="默认 180（3 分钟）"
                value={draft.fingerprint_duration_secs ?? ""}
                onChange={(e) => {
                  const raw = e.target.value;
                  const parsed = raw === "" ? null : Number(raw);
                  set("fingerprint_duration_secs", parsed);
                }}
              />
              <p className="text-caption text-white/40">
                从每集开头截取多长音频做声纹比对以定位片头。越长越精确但越费流量：本地文件建议 180~300 秒；
                STRM / 网盘挂载建议 90~180 秒，避免拉取过多音频流。留空跟随默认 180 秒。
              </p>
            </div>
          </Group>
        </div>
      )}

      <div className="mt-8 flex items-center gap-3">
        <button
          type="button"
          onClick={save}
          disabled={busy || !dirty}
          className="rounded-xl bg-white/15 px-5 py-2.5 text-ui font-medium text-white transition hover:bg-white/25 disabled:opacity-40"
        >
          {busy ? "保存中…" : "保存设置"}
        </button>
        {!dirty && <span className="text-caption text-white/40">没有未保存的修改</span>}
      </div>
    </div>
  );
}

function Group({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <section className="rounded-2xl border border-white/[0.08] bg-white/[0.03] p-4">
      <h3 className="group-label mb-3 text-sub font-semibold text-white/70">{label}</h3>
      {children}
    </section>
  );
}

function PriorityEditor({
  value,
  onChange,
  options,
  placeholder,
  allowCustom = false,
  customValue,
  onCustomChange,
  searchable = false,
}: {
  value: string[];
  onChange: (next: string[]) => void;
  options: { value: string; label: string; hint?: string }[];
  placeholder: string;
  allowCustom?: boolean;
  customValue?: string;
  onCustomChange?: (v: string) => void;
  /** 候选区追加「更多」搜索（完整语种/地区表按输入过滤）。 */
  searchable?: boolean | "countries";
}) {
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [extended, setExtended] = useState<{ value: string; label: string }[]>([]);
  const labelOf = (v: string) =>
    options.find((o) => o.value === v)?.label ??
    extended.find((o) => o.value === v)?.label ??
    (v === "" ? "无文字" : v);
  const available = options.filter((o) => !value.includes(o.value));
  const extendedAll = useMemo(
    () => extended.filter((o) => !value.includes(o.value) && !options.some((p) => p.value === o.value)),
    [extended, options, value],
  );
  const matches = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return extendedAll.slice(0, 30);
    return extendedAll
      .filter((o) => o.value.toLowerCase().includes(q) || o.label.toLowerCase().includes(q))
      .slice(0, 30);
  }, [query, extendedAll]);

  useEffect(() => {
    if (!searchable) return;
    const loader = searchable === "countries" ? fetchCountryCandidates : fetchLanguageCandidates;
    void loader()
      .then((rows) =>
        setExtended(
          rows.map((row) => ({ value: row.code, label: row.name || row.english_name || row.code })),
        ),
      )
      .catch(() => undefined);
  }, [searchable]);
  return (
    <div className="space-y-2.5">
      <div className="flex flex-wrap gap-1.5">
        {value.map((v, i) => (
          <span
            key={`${v}-${i}`}
            title={options.find((o) => o.value === v)?.hint}
            className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-caption font-medium ${
              i === 0
                ? "border-[var(--accent)]/40 bg-[var(--accent)]/15 text-white"
                : "border-white/10 bg-white/[0.06] text-white/80"
            }`}
          >
            <span className="tnum text-white/40">{i + 1}</span>
            {labelOf(v)}
            {i === 0 && <span className="text-micro text-white/50">主</span>}
            <button
              type="button"
              aria-label={`移除 ${labelOf(v)}`}
              onClick={() => onChange(value.filter((x) => x !== v))}
              className="rounded-full p-0.5 text-white/40 transition hover:bg-white/10 hover:text-white"
            >
              <XIcon className="size-3" />
            </button>
          </span>
        ))}
        {value.length === 0 && <span className="text-caption text-white/35">未设置（跟随默认）</span>}
      </div>
      {available.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5">
          {available.map((o) => (
            <button
              key={o.value}
              type="button"
              title={o.hint}
              onClick={() => onChange([...value, o.value])}
              className="inline-flex items-center gap-1 rounded-full border border-white/10 px-2.5 py-1 text-caption text-white/60 transition hover:border-white/25 hover:text-white"
            >
              <PlusIcon className="size-3" />
              {o.label}
            </button>
          ))}
          {allowCustom && onCustomChange && (
            <input
              value={customValue}
              onChange={(e) => onCustomChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && customValue?.trim()) {
                  onChange([...value, customValue.trim()]);
                  onCustomChange("");
                }
              }}
              placeholder={placeholder}
              className="w-28 rounded-full border border-white/10 bg-transparent px-2.5 py-1 text-caption outline-none placeholder:text-white/25 focus:border-white/30"
            />
          )}
        </div>
      )}
    </div>
  );
}

function NumberInput({
  value,
  onChange,
  placeholder,
}: {
  value: number | null;
  onChange: (v: number | null) => void;
  placeholder: string;
}) {
  return (
    <input
      type="number"
      min={0}
      className={INPUT_CLASS}
      value={value ?? ""}
      placeholder={placeholder}
      onChange={(e) => onChange(e.target.value === "" ? null : Number(e.target.value))}
    />
  );
}

function SizeSelect({
  value,
  onChange,
  options,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  options: string[];
  placeholder: string;
}) {
  return (
    <select className={INPUT_CLASS} value={value || "__default__"} onChange={(e) => onChange(e.target.value === "__default__" ? "" : e.target.value)}>
      <option value="__default__">默认（{placeholder}）</option>
      {options.map((o) => (
        <option key={o} value={o}>
          {o}
        </option>
      ))}
    </select>
  );
}

function TemplateField({
  label,
  value,
  onChange,
  placeholder,
  tokens,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  tokens: string[];
}) {
  return (
    <div>
      <label className={LABEL_CLASS}>{label}</label>
      <input className={INPUT_CLASS} value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} />
      <div className="mt-1.5 flex flex-wrap gap-1">
        {tokens.map((t) => (
          <button
            key={t}
            type="button"
            onClick={() => onChange(value ? `${value}${t}` : t)}
            className="rounded-md border border-white/10 px-1.5 py-0.5 font-mono text-micro text-white/55 transition hover:border-white/25 hover:text-white"
          >
            {t}
          </button>
        ))}
      </div>
    </div>
  );
}

function ToggleRow({
  label,
  hint,
  value,
  onChange,
}: {
  label: string;
  hint: string;
  value: boolean | null;
  onChange: (v: boolean | null) => void;
}) {
  return (
    <div className="flex items-start justify-between gap-4 py-3 first:pt-0 last:pb-0">
      <div className="min-w-0">
        <p className="text-ui font-medium text-white/85">{label}</p>
        <p className="mt-0.5 text-caption leading-5 text-white/40">{hint}</p>
      </div>
      <div className="flex shrink-0 items-center gap-2">
        <select
          className="rounded-lg border border-white/10 bg-white/[0.04] px-2 py-1 text-caption text-white/80 outline-none"
          value={value === null ? "default" : value ? "on" : "off"}
          onChange={(e) => onChange(e.target.value === "default" ? null : e.target.value === "on")}
        >
          <option value="default">跟随默认（开）</option>
          <option value="on">开</option>
          <option value="off">关</option>
        </select>
        {value !== null && (
          <span className={value ? "text-[var(--ok)]" : "text-[var(--danger)]"}>
            <CheckIcon className="size-4" />
          </span>
        )}
      </div>
    </div>
  );
}
