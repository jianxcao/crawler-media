"use client";
import { useCallback, useEffect, useState } from "react";
import { HttpError } from "@/lib/http";
import { getMetadataLanguage, saveMetadataLanguage, retryMetadataLanguageOperation, type MetadataLanguageState, type MetadataLanguageOperation } from "@/lib/api/metadata-language";

export function MetadataLanguageSetting() {
  const [state, setState] = useState<MetadataLanguageState | null>(null);
  const [language, setLanguage] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const reload = useCallback(async (reset = false) => {
    const value = await getMetadataLanguage();
    setState(value);
    if (reset) setLanguage(value.language);
  }, []);
  useEffect(() => { void reload(true).catch(e => setError(String(e))); }, [reload]);
  const pending = state?.operations.some(op => op.state !== "succeeded" && op.state !== "failed");
  // A retry can still read the preceding failure before its driver starts. Keep
  // observing every unfinished child; failed reads never latch the Retry button.
  const unfinished = state?.operations.some(op => op.state !== "succeeded");
  useEffect(() => {
    if (!unfinished) return;
    const timer = setInterval(() => { void reload().catch(e => setError(String(e))); }, 1500);
    return () => clearInterval(timer);
  }, [unfinished, reload]);
  async function save() {
    if (!state) return;
    setBusy(true); setError("");
    try { const value = await saveMetadataLanguage(state.revision, language); setState(value); setLanguage(value.language); }
    catch (e) { setError(String(e)); if (e instanceof HttpError && e.status === 409) { await reload(true).catch(e => setError(String(e))); } }
    finally { setBusy(false); }
  }
  async function retry(operation: MetadataLanguageOperation) {
    setBusy(true); setError("");
    try { await retryMetadataLanguageOperation(operation); await reload(); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  return <section className="css-glass rounded-2xl p-6 space-y-3">
    <h2>默认元数据语言</h2>
    <p>保存后会逐个重建媒体库，完成前暂时无法浏览。元数据来源使用的默认语言将在服务重启后生效；留空时使用 zh-CN。</p>
    <label>语言 <input aria-label="默认元数据语言" value={language} onChange={e => setLanguage(e.target.value)} placeholder="未设置（如 en-US）" disabled={!state || busy || pending} /></label>
    <button onClick={() => void save()} disabled={!state || busy || pending || state.operations.some(op => op.state === "failed")}>保存语言</button>
    {pending && <p role="status">媒体库重建中…</p>}
    {state?.operations.map(op => <div key={op.operation_id}><span>{op.library_id}：{operationLabel(op.state)}</span>{op.error && <p role="alert">{op.error}</p>}{op.state === "failed" && <button disabled={busy} onClick={() => void retry(op)}>重试</button>}</div>)}
    {error && <p role="alert">{error}</p>}
  </section>;
}

function operationLabel(state: string): string {
  switch (state) {
    case "succeeded": return "已完成";
    case "failed": return "重建失败";
    case "applying": return "正在重建";
    default: return "等待重建";
  }
}
