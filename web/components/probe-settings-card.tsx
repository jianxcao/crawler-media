import { useEffect, useState } from "react";
import {
  getProbeSettings, getRemoteReadsSettings, saveProbeSettings, saveRemoteReadsSettings,
  type ProbeSettings,
} from "@/lib/api/probe-settings";

const FIELDS: { key: keyof ProbeSettings; label: string; min: number; max: number; hint: string }[] = [
  { key: "metadata_workers", label: "媒体信息并发", min: 1, max: 8, hint: "同时抓取媒体流信息的文件数，默认 1。" },
  { key: "remote_concurrency", label: "远程读取并发", min: 1, max: 8, hint: "同时读取 STRM 等远程媒体的数量，默认 1。" },
  { key: "remote_cooldown_ms", label: "远程读取间隔（毫秒）", min: 0, max: 60000, hint: "远程读取完成后到下一次读取的间隔，默认 500；0 表示不等待。" },
];

function RemoteReadsControl() {
  const [paused, setPaused] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    let active = true;
    void getRemoteReadsSettings().then(settings => {
      if (active) setPaused(settings.paused);
    }).catch((cause: unknown) => {
      if (active) setError(`读取 STRM 暂停状态失败：${(cause as Error).message}`);
    });
    return () => { active = false; };
  }, []);

  const toggle = async () => {
    if (paused === null || busy) return;
    setBusy(true);
    setError("");
    try {
      const settings = await saveRemoteReadsSettings(!paused);
      setPaused(settings.paused);
    } catch (cause) {
      setError(`保存 STRM 暂停状态失败：${(cause as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-4 border-t border-white/[0.08] pt-4">
      <p className="text-caption leading-5 text-white/55">
        暂停后，新的 STRM 媒体信息、声纹、图片和章节后台读取会等待恢复。
        已开始的读取可完成，暂停状态会在重启后保留。
      </p>
      {paused !== null && (
        <div className="mt-3 flex flex-wrap items-center gap-3">
          <button type="button" onClick={toggle} disabled={busy}
            className="rounded-xl bg-white/15 px-4 py-2 text-ui font-medium text-white transition hover:bg-white/25 disabled:opacity-40">
            {busy ? "保存中…" : paused ? "恢复 STRM 后台读取" : "暂停 STRM 后台读取"}
          </button>
          <p role="status" className="text-caption text-white/60">
            {paused ? "已暂停" : "已启用"}
          </p>
        </div>
      )}
      {error && <p role="alert" className="mt-2 text-caption text-[var(--danger)]">{error}</p>}
    </div>
  );
}

export function ProbeSettingsCard() {
  const [saved, setSaved] = useState<ProbeSettings | null>(null);
  const [draft, setDraft] = useState<ProbeSettings | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");

  useEffect(() => {
    let active = true;
    void getProbeSettings().then(settings => {
      if (active) { setSaved(settings); setDraft(settings); }
    }).catch((cause: unknown) => {
      if (active) setError(`读取探测设置失败：${(cause as Error).message}`);
    });
    return () => { active = false; };
  }, []);

  const invalid = !draft || FIELDS.some(field => {
    const value = draft[field.key];
    return !Number.isInteger(value) || value < field.min || value > field.max;
  });
  const dirty = JSON.stringify(saved) !== JSON.stringify(draft);
  const save = async () => {
    if (!draft || invalid || busy) return;
    setBusy(true); setError(""); setMessage("");
    try {
      const settings = await saveProbeSettings(draft);
      setSaved(settings); setDraft(settings);
      setMessage("探测设置已保存，后续任务立即生效；正在读取的媒体会继续完成。");
    } catch (cause) {
      setError(`保存探测设置失败：${(cause as Error).message}`);
    } finally { setBusy(false); }
  };

  return (
    <section className="rounded-2xl border border-white/[0.08] bg-white/[0.02] p-5">
      <h3 className="text-sub font-medium text-white/85">媒体信息探测</h3>
      <p className="mt-2 text-caption leading-5 text-white/45">
        根据设备性能和远程媒体服务的限制调整抓取速度。保存后立即生效。
      </p>
      <RemoteReadsControl />
      {draft ? (
        <div className="mt-4 grid gap-4 lg:grid-cols-3">
          {FIELDS.map(field => (
            <label key={field.key} className="block text-sub text-white/75">
              {field.label}
              <input
                type="number" aria-label={field.label} min={field.min} max={field.max} step={1}
                value={Number.isNaN(draft[field.key]) ? "" : draft[field.key]}
                onChange={event => {
                  const value = event.target.value === "" ? Number.NaN : Number(event.target.value);
                  setDraft(current => current && ({ ...current, [field.key]: value }));
                  setMessage("");
                }}
                className="mt-2 w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
              />
              <span className="mt-2 block text-caption leading-5 text-white/40">
                {field.hint} 范围 {field.min}–{field.max}。
              </span>
            </label>
          ))}
        </div>
      ) : !error && <p className="mt-4 text-caption text-white/45">正在读取探测设置…</p>}
      <div className="mt-4 flex flex-wrap items-center gap-3">
        <button type="button" onClick={save} disabled={busy || invalid || !dirty}
          className="rounded-xl bg-white/15 px-4 py-2 text-ui font-medium text-white transition hover:bg-white/25 disabled:opacity-40">
          {busy ? "保存中…" : "保存探测设置"}
        </button>
        {message && <p role="status" className="text-caption text-white/60">{message}</p>}
        {error && <p role="alert" className="text-caption text-[var(--danger)]">{error}</p>}
      </div>
    </section>
  );
}
