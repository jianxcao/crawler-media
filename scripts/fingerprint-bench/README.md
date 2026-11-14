# Current production timing

Build before the measurement; compilation and source verification are outside the Media clock. Run from the repository root, with an isolated evaluation data directory:

```sh
python3 scripts/with-build-lock.py --exclusive cargo build --release -j8 -p api --bin fingerprint-bench
CM_EVAL_DIR=$(mktemp -d /tmp/cm-fingerprint-XXXXXX)
python3 scripts/with-build-lock.py --exclusive target/release/fingerprint-bench \
  --manifest /absolute/manifest.json --sources /absolute/sources.json \
  --mode full_window --capture-policy recapture --scenario cold \
  --data-dir "$CM_EVAL_DIR/data" --output "$CM_EVAL_DIR/runs" --run-id current-cold
```

`--mode` accepts `full_window` or `adaptive`. Every source row requires explicit production coordinates:

```json
[{"case_id":"opaque-intro-id","path":"/absolute/episode.mp4","audio_stream_index":1,"unit":{"season":1,"episode":1}}]
```

The evaluation manifest supplies Media grouping, duration, kind and complete local file SHA256. Intro/outro mappings for one file must agree; case IDs and filenames do not supply identity. Separate cuts may share an episode. Whole-work units are unsupported. STRM sources emit unverified/error predictions even with `--allow-remote`; neither STRM text nor sampled PCM verifies a complete remote source.

`cold` requires newly imported sources; `warm` requires existing sources. `refresh` explicitly applies `reuse_valid` or `recapture`; `incremental` requires existing files plus new arrivals and queues only arrivals. `retry` requests production manual retry, including failed scene frames. `incremental` and `retry` require `--capture-policy reuse_valid`, matching their natural production reuse of valid windows; `recapture` is rejected before importing sources or starting work. Existing source IDs stay stable across reordered or extended inventories. Unmarked, nonempty instance directories are refused. Media imports and all sidecars stay inside the isolated evaluation directory.

Each stdout row and JSON `media` record reports episode count, file count, elapsed time and terminal status. The clock begins before enqueue, includes queue waits, recognition and publication, and ends after probe dispatch release and every required scene JPEG reaches terminal state and decodes successfully. Failed/incomplete runs retain elapsed time and return failure. Source hashing, isolated import, tool discovery and teardown are outside that clock. Cold application state does not imply a cold OS cache.

`<run-id>.json` contains build/tool provenance, capture attempts, jobs, frame identities and predictions. Stages are observed operation sums, may overlap across workers, and cannot be added into wallclock. Unavailable stages and input traffic remain `null`; `measurement_complete` means traffic completeness, while `wallclock_complete` means successful lifecycle completion. Absence without a persisted typed result remains `uncertain` in predictions. Timing alone makes no speedup claim. The optional pairing utility rejects mismatched/incomplete pairs and labels fewer than five pairs as a pilot.

See the [accuracy and timing runbook](../../docs/testing/marker-accuracy-runbook.md) for independent labels, source/audio/timeline verification, per-kind outcomes and real JPEG checks. Deliver needed reports before removing the evaluation directory; do not remove input media or shared build caches.
