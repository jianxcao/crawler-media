/* DOM binding only; review state and media operations live in annotation-review.js. */
(function () {
  'use strict';
  const { ReviewSession, LocalPlayback, hashLocalFile } = globalThis.MarkerReview;
  const $ = id => document.getElementById(id);
  const video = $('video');
  const player = new LocalPlayback(video);
  let session = null, selectedFile = null, generation = 0, playGeneration = 0, playable = false;
  const showError = error => { $('error').textContent = error?.message || String(error); };
  function clearSource(resetInput = true) {
    generation++; selectedFile = null; playable = false; session?.clearSource(); player.dispose();
    if (resetInput) $('video-file').value = '';
    $('external-hash').value = '';
    $('external-confirm').checked = false; $('audio-confirm').checked = false;
    $('audio-note').value = ''; $('review-confirm').checked = false;
    $('source-status').textContent = '尚未选择视频。';
  }
  function render() {
    const view = session?.view();
    $('cases').disabled = !view?.case;
    $('identity').textContent = view ? JSON.stringify({ corpus_version: view.corpus_version,
      evidence_kind: view.evidence_kind, case: view.case, evidence: view.evidence,
      source_verification: view.sourceVerification, audio_declaration: view.audioDeclaration,
      browser_audio_track: 'unknown; not independently verified' }, null, 2) : '尚未加载。';
    $('label').textContent = view?.label ? JSON.stringify(view.label, null, 2) : '标签未加载：insufficient。';
    $('prediction-panel').hidden = !view?.revealed;
    $('prediction').textContent = view?.revealed ? JSON.stringify(view.prediction, null, 2) : '';
    $('errors').textContent = view?.errors
      ? `起点 signed error: ${view.errors.start_ms} ms；终点 signed error: ${view.errors.end_ms} ms`
      : 'signed error: N/A（无可比边界）';
    for (const origin of ['label', 'prediction']) for (const edge of ['start', 'end'])
      $(origin + '-' + edge).disabled = !(playable && view?.sourceVerified
        && (origin === 'label' ? view.label?.state === 'present' : view.prediction?.state === 'matched'));
    $('external-verify').disabled = !selectedFile || !view?.case;
    $('complete').disabled = !(playable && view?.sourceVerified && view?.label
      && $('audio-confirm').checked && $('audio-note').value.trim() && $('review-confirm').checked);
    $('reveal').disabled = !view?.reviewComplete;
    $('review-status').textContent = view?.revealed ? '当前 case 已揭示；不能再视为新的独立盲标。'
      : view?.reviewComplete ? '复核已完成，预测仍隐藏；可显式揭示。' : '预测隐藏。';
  }
  function action(id, callback, event = 'click') {
    $(id).addEventListener(event, async () => {
      $('error').textContent = '';
      try { await callback(); } catch (error) { showError(error); }
      render();
    });
  }
  async function readJSON(id, required) {
    const file = $(id + '-file').files[0];
    if (!file) { if (required) throw new Error('请选择 ' + id + ' JSON'); return []; }
    if (file.size > 8 * 1024 * 1024) throw new Error(id + ' JSON 超过本工具 8 MiB 上限');
    try { return JSON.parse(await file.text()); }
    catch { throw new Error(id + ' JSON 无法读取/解析；请检查格式（预测内容仍隐藏）。'); }
  }
  action('load', async () => {
    clearSource(); session = null; $('cases').replaceChildren(); render();
    const token = generation;
    const inputs = await Promise.all([readJSON('manifest', true), readJSON('labels'), readJSON('predictions')]);
    if (token !== generation) return;
    session = new ReviewSession(...inputs);
    for (const c of session.view().cases) {
      const option = document.createElement('option'); option.value = c.case_id;
      option.textContent = `${c.case_id} · ${c.kind} · ${c.media_group}`; $('cases').append(option);
    }
    if (!session.view().case) showError(new Error('没有 case：insufficient。'));
  });
  action('cases', () => {
    // Always unbind, even if the next kind shares the same source: no stale asynchronous verification.
    clearSource(); session.select($('cases').value);
  }, 'change');
  action('video-file', async () => {
    const file = $('video-file').files[0]; clearSource(false); render();
    if (!file) return;
    if (!session?.view().case) throw new Error('请先加载 manifest 并选择 case。');
    selectedFile = file; const token = generation;
    $('source-status').textContent = `所选文件：${file.name} (${file.size} bytes)；正在计算完整 SHA-256…`;
    try {
      const hash = await hashLocalFile(file);
      if (token !== generation) return;
      session.verifySource(hash); player.replace(file);
      $('source-status').textContent = `浏览器已计算并匹配完整 SHA-256：${hash}；文件：${file.name} (${file.size} bytes)`;
    } catch (error) { if (token === generation) {
      $('source-status').textContent = `源未验证：${file.name} (${file.size} bytes)。需要正确完整源或人工外部核验声明。`;
      showError(error);
    } }
  }, 'change');
  action('external-verify', () => {
    if (!selectedFile || !$('external-confirm').checked) throw new Error('请先选文件并明确确认本机完整文件哈希核验。');
    const hash = $('external-hash').value.trim();
    if (!/^sha256:[0-9a-f]{64}$/.test(hash)) throw new Error('完整文件哈希必须为 sha256: 加 64 位小写十六进制。');
    generation++; playable = false; player.dispose();
    session.verifySource(hash, 'external hash attestation'); player.replace(selectedFile);
    $('source-status').textContent = `人工外部核验声明（浏览器未计算）：${hash}；文件：${selectedFile.name} (${selectedFile.size} bytes)`;
    $('review-confirm').checked = false;
  });
  action('complete', () => {
    if (!playable || !$('audio-confirm').checked || !$('review-confirm').checked) throw new Error('请完成播放、音轨核验与标签复核。');
    session.confirmAudio($('audio-note').value.trim()); session.completeReview();
  });
  action('reveal', () => session.reveal());
  for (const origin of ['label', 'prediction']) for (const edge of ['start', 'end'])
    action(origin + '-' + edge, async () => {
      const operation = ++playGeneration, token = generation;
      const playingSession = session, source = selectedFile, url = video.getAttribute('src');
      try { await player.playContext(playingSession.context(origin, edge)); }
      catch (error) {
        if (operation !== playGeneration || token !== generation || session !== playingSession
          || selectedFile !== source || video.getAttribute('src') !== url) return;
        session.clearSource(); playable = false; player.dispose(); showError(error);
      }
    });
  for (const id of ['audio-confirm', 'audio-note', 'review-confirm']) action(id, () => {
    // Any amended declaration invalidates the completion gate, without erasing prior reveal knowledge.
    if (session?.view().reviewComplete) {
      const current = $('cases').value; session.select(current);
    }
  }, 'input');
  video.addEventListener('loadedmetadata', () => {
    if (!session?.view().sourceVerified || !selectedFile) return;
    const expected = session.view().case.duration_ms / 1000;
    if (!Number.isFinite(video.duration) || Math.abs(video.duration - expected) > 1) {
      session.clearSource(); playable = false;
      showError(new Error(`视频时长与 manifest 不匹配：浏览器 ${video.duration}s / manifest ${expected}s。请检查源与原始时间线。`));
      player.dispose();
    } else playable = true;
    render();
  });
  video.addEventListener('error', () => {
    if (!selectedFile || !video.getAttribute('src')) return;
    const code = video.error?.code;
    playable = false; session?.clearSource(); player.dispose();
    showError(new Error(`本地视频无法读取/解码（MediaError ${code ?? 'unknown'}）。请按 runbook 使用本地播放器或经时间线校验的单音轨评测副本。`)); render();
  });
  video.addEventListener('timeupdate', () => {
    $('clock').textContent = `原始媒体时间：${video.currentTime.toFixed(3)} 秒`; player.tick();
  });
  window.addEventListener('pagehide', () => { generation++; player.dispose(); });
  render();
})();
