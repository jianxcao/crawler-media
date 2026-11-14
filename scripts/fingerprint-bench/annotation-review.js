/* No network or persistence. The same public interfaces run in file:// and Node tests. */
(function (root) {
  'use strict';
  const CASE = ['case_id', 'media_group', 'split', 'source_hash', 'duration_ms', 'kind'];
  const LABEL = ['case_id', 'source_hash', 'state', 'safe_intervals_ms', 'protected_intervals_ms',
    'start_range_ms', 'end_range_ms', 'reviewers', 'adjudicated'];
  const PREDICTION = ['case_id', 'source_hash', 'state', 'interval_ms', 'reason', 'run_id'];
  const text = value => typeof value === 'string' && value.trim().length > 0;
  const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
  const requireThat = (condition, message) => { if (!condition) throw new Error(message); };
  const exact = (value, fields) => object(value) && Object.keys(value).length === fields.length
    && fields.every(key => Object.hasOwn(value, key));
  const interval = (value, duration, equal = false) => Array.isArray(value) && value.length === 2
    && value.every(Number.isSafeInteger) && value[0] >= 0 && value[1] <= duration
    && (equal ? value[0] <= value[1] : value[0] < value[1]);
  const signed = (value, range) => value < range[0] ? value - range[0] : value > range[1] ? value - range[1] : 0;
  function validatePolicy(policy) {
    requireThat(object(policy), 'invalid acceptance_policy');
    for (const [key, value] of Object.entries(policy)) {
      let valid = false;
      if (key === 'frozen') valid = typeof value === 'boolean';
      if (['min_sources', 'min_media_groups', 'min_holdout_sources', 'min_holdout_media_groups', 'bootstrap_samples'].includes(key))
        valid = Number.isSafeInteger(value) && value > 0;
      if (key === 'bootstrap_seed') valid = Number.isSafeInteger(value);
      if (['min_clear_fraction', 'min_precision', 'min_recall', 'min_boundary_2s'].includes(key))
        valid = typeof value === 'number' && value > 0 && value <= 1;
      if (key === 'required_kinds') valid = Array.isArray(value) && value.length > 0
        && value.every(x => ['intro', 'outro'].includes(x)) && new Set(value).size === value.length;
      requireThat(valid, 'invalid acceptance_policy');
    }
  }
  function validateManifest(manifest) {
    requireThat(object(manifest) && text(manifest.corpus_version) && Array.isArray(manifest.cases)
      && Object.keys(manifest).every(x => ['corpus_version', 'cases', 'evidence_kind', 'acceptance_policy'].includes(x))
      && ['synthetic', 'human_reviewed', 'public_annotation'].includes(manifest.evidence_kind), 'invalid manifest');
    if (Object.hasOwn(manifest, 'acceptance_policy')) validatePolicy(manifest.acceptance_policy);
    const ids = new Set(), identities = new Set(), groups = new Map(), sources = new Map();
    for (const c of manifest.cases) {
      requireThat(exact(c, CASE) && ['case_id', 'media_group', 'source_hash'].every(key => text(c[key]))
        && ['development', 'validation', 'holdout'].includes(c.split) && ['intro', 'outro'].includes(c.kind)
        && Number.isSafeInteger(c.duration_ms) && c.duration_ms > 0, 'invalid case');
      const identity = JSON.stringify([c.source_hash, c.kind]);
      const source = JSON.stringify([c.media_group, c.split, c.duration_ms]);
      requireThat(!ids.has(c.case_id) && !identities.has(identity), 'duplicate case/source kind');
      requireThat(!groups.has(c.media_group) || groups.get(c.media_group) === c.split, 'Media split leakage');
      requireThat(!sources.has(c.source_hash) || sources.get(c.source_hash) === source, 'inconsistent source');
      ids.add(c.case_id); identities.add(identity); groups.set(c.media_group, c.split); sources.set(c.source_hash, source);
    }
  }
  function validateLabel(c, l) {
    const safe = l.safe_intervals_ms, protectedRanges = l.protected_intervals_ms;
    requireThat(['present', 'absent', 'ambiguous'].includes(l.state) && Array.isArray(safe)
      && Array.isArray(protectedRanges) && [...safe, ...protectedRanges].every(x => interval(x, c.duration_ms))
      && Array.isArray(l.reviewers) && l.reviewers.every(text) && new Set(l.reviewers).size >= 2
      && typeof l.adjudicated === 'boolean', 'invalid label');
    if (l.state === 'present') {
      requireThat(safe.length > 0 && l.adjudicated && interval(l.start_range_ms, c.duration_ms, true)
        && interval(l.end_range_ms, c.duration_ms, true) && l.start_range_ms[1] < l.end_range_ms[0], 'invalid label boundaries');
    } else requireThat(!safe.length && l.start_range_ms === null && l.end_range_ms === null
      && (l.state !== 'absent' || l.adjudicated), 'invalid label state');
    requireThat(!safe.some(a => protectedRanges.some(b => Math.min(a[1], b[1]) > Math.max(a[0], b[0]))), 'overlapping truth');
  }
  function validateRows(cases, rows, kind) {
    requireThat(Array.isArray(rows), 'invalid ' + kind);
    const result = new Map();
    for (const row of rows) {
      requireThat(exact(row, kind === 'label' ? LABEL : PREDICTION) && text(row.case_id), 'invalid ' + kind);
      const c = cases.find(x => x.case_id === row.case_id);
      requireThat(c && !result.has(row.case_id) && row.source_hash === c.source_hash, kind + ' identity mismatch/duplicate');
      if (kind === 'label') validateLabel(c, row);
      else requireThat(['matched', 'no_match', 'uncertain', 'error'].includes(row.state)
        && text(row.reason) && text(row.run_id) && (row.state === 'matched'
          ? interval(row.interval_ms, c.duration_ms) : row.interval_ms === null), 'invalid prediction');
      result.set(row.case_id, row);
    }
    requireThat(!rows.length || result.size === cases.length, 'missing ' + kind);
    return result;
  }
  class ReviewSession {
    #manifest; #labels; #predictions; #case; #source = null; #audio = null; #complete = false; #revealed = false;
    constructor(manifest, labels = [], predictions = []) {
      [manifest, labels, predictions] = structuredClone([manifest, labels, predictions]);
      validateManifest(manifest);
      this.#manifest = manifest;
      this.#labels = validateRows(manifest.cases, labels, 'label');
      this.#predictions = validateRows(manifest.cases, predictions, 'prediction');
      this.#case = manifest.cases[0] || null;
    }
    select(id) {
      const next = this.#manifest.cases.find(c => c.case_id === id);
      requireThat(next, 'unknown case');
      if (next.source_hash !== this.#case?.source_hash) this.clearSource();
      this.#case = next; this.#complete = false; this.#revealed = false;
    }
    clearSource() { this.#source = null; this.#audio = null; this.#complete = false; this.#revealed = false; }
    verifySource(hash, method = 'browser SHA-256') {
      this.clearSource();
      requireThat(this.#case && hash === this.#case.source_hash, 'source hash mismatch');
      requireThat(['browser SHA-256', 'external hash attestation'].includes(method), 'unknown verification method');
      this.#source = method;
    }
    confirmAudio(note) {
      requireThat(this.#source && text(note), 'source and audio verification required');
      this.#audio = note; this.#complete = false; this.#revealed = false;
    }
    completeReview() {
      requireThat(this.#source, 'source verification required');
      requireThat(this.#audio, 'audio verification required');
      requireThat(this.#labels.has(this.#case.case_id), 'label missing: insufficient');
      this.#complete = true;
    }
    reveal() {
      requireThat(this.#complete, 'complete review before reveal');
      this.#revealed = true;
    }
    view() {
      const label = this.#labels.get(this.#case?.case_id) || null;
      const prediction = this.#revealed ? this.#predictions.get(this.#case?.case_id) || null : null;
      const errors = prediction?.state === 'matched' && label?.state === 'present'
        ? { start_ms: signed(prediction.interval_ms[0], label.start_range_ms), end_ms: signed(prediction.interval_ms[1], label.end_range_ms) } : null;
      return structuredClone({ case: this.#case, cases: this.#manifest.cases, corpus_version: this.#manifest.corpus_version,
        evidence_kind: this.#manifest.evidence_kind, evidence: label ? 'loaded labels; not an accuracy verdict' : 'insufficient',
        label, prediction, errors, sourceVerified: this.#source !== null, sourceVerification: this.#source,
        audioDeclaration: this.#audio, reviewComplete: this.#complete, revealed: this.#revealed });
    }
    context(origin, boundary) {
      requireThat(['label', 'prediction'].includes(origin) && ['start', 'end'].includes(boundary), 'invalid boundary');
      const view = this.view();
      requireThat(origin !== 'prediction' || view.revealed, 'prediction hidden');
      const row = view[origin];
      requireThat(row && (origin === 'label' ? row.state === 'present' : row.state === 'matched'), 'no boundary');
      const range = origin === 'label' ? row[boundary + '_range_ms'] : [row.interval_ms[boundary === 'start' ? 0 : 1]];
      return { start_ms: Math.max(0, range[0] - 10000), end_ms: Math.min(view.case.duration_ms, range.at(-1) + 10000) };
    }
  }
  class LocalPlayback {
    #video; #urls; #url = null; #end = null; #generation = 0;
    constructor(video, urls = URL) { this.#video = video; this.#urls = urls; }
    replace(file) { this.dispose(); this.#url = this.#urls.createObjectURL(file); this.#video.src = this.#url; this.#video.load(); }
    async playContext(context) {
      const generation = ++this.#generation;
      requireThat(this.#url, 'select local video first');
      this.#video.currentTime = context.start_ms / 1000; this.#end = context.end_ms / 1000;
      this.#video.muted = false; this.#video.volume = 1;
      try { await this.#video.play(); } catch (error) {
        if (generation !== this.#generation) return;
        this.#end = null; throw new Error('Playback failed: ' + error.message);
      }
    }
    tick() { if (this.#end !== null && this.#video.currentTime >= this.#end) { this.#video.pause(); this.#end = null; } }
    dispose() {
      this.#generation++;
      this.#video.pause(); this.#video.removeAttribute('src'); this.#video.load();
      if (this.#url) this.#urls.revokeObjectURL(this.#url);
      this.#url = null; this.#end = null;
    }
  }
  async function hashLocalFile(file, cryptoProvider = globalThis.crypto) {
    requireThat(file.size <= 128 * 1024 * 1024, 'Browser hash limit: 128 MiB. Use shasum -a 256 and external hash attestation (see runbook).');
    requireThat(cryptoProvider?.subtle, 'WebCrypto unavailable: use external hash attestation (see runbook).');
    const digest = await cryptoProvider.subtle.digest('SHA-256', await file.arrayBuffer());
    return 'sha256:' + Array.from(new Uint8Array(digest), x => x.toString(16).padStart(2, '0')).join('');
  }
  const api = { ReviewSession, LocalPlayback, hashLocalFile };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.MarkerReview = api;
})(globalThis);
