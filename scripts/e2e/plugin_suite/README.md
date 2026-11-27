# crawler-media Plugin System Opaque-Box E2E Suite

Pure Python 3.9+ stdlib opaque-box end-to-end test infrastructure for Features #1–#26 of the `crawler-media` plugin system.

## Design & Isolation Guarantees

- **Opaque-box only**: Zero Rust crate imports and zero direct SQLite inspection. All tests interact exclusively via HTTP (`/api/v1/...`), CLI (`crawler-media`), `.cmplugin` / `.minisig` / `.torrent` artifacts, and local mock servers.
- **Python 3.9.6 stdlib only**: No third-party packages (`cryptography`, `nacl`, `tomllib`, etc.). Includes pure-Python RFC 8032 Ed25519 + BLAKE2b Minisign signing/verification, canonical bencode `.torrent` builder, deterministic `.cmplugin` ZIP + structural attack generator, and RFC 6455 WebSocket CDP mock.
- **Strict workspace isolation**:
  - Each run allocates an isolated `/tmp/crawler-e2e-XXXXXX` root (`data/`, `logs/`) and ephemeral loopback ports (`127.0.0.1`).
  - Ambient `CRAWLER_MEDIA_*` environment variables are stripped before spawning the backend or CLI.
  - Never mutates `~/Downloads/crawler-media/data/` or reaches external internet, real qBittorrent, real TMDB, or real Chromium.
  - Temporary directories are removed automatically on exit unless `--keep-tmp` is passed.
- **Explicit failure on unimplemented endpoints**: If a target backend returns `404`/`501` for a Phase 1 endpoint, the runner records a `FAIL` (`unimplemented: true`) with diagnostics — never a silent skip or fake pass.

## Usage

### 1. Self-Test Mode (In-Process Stub Server)

Validates suite-wide registry coverage thresholds (`>= 130` T1, `>= 130` T2, `>= 26` T3, `>= 13` T4, `>= 299` non-smoke, `>= 303` total), `ApiClient`, `fixtures.py`, `mocks.py`, tier dispatch, and JSON reporting without needing a compiled/completed backend:

```sh
python3 scripts/e2e/plugin_suite/run.py --self-test --json-report /tmp/e2e_self_test.json
```

### 2. Spawn an Isolated Backend Instance

Spawns `<repo>/target/debug/crawler-media serve` (or `--bin <path>`) with an isolated `/tmp/crawler-e2e-*` `DATA_DIR`:

```sh
python3 scripts/e2e/plugin_suite/run.py --spawn-backend --all
python3 scripts/e2e/plugin_suite/run.py --spawn-backend --tier 1 --feature 8
python3 scripts/e2e/plugin_suite/run.py --spawn-backend --tier 2 --web-dir web/dist --json-report /tmp/report.json
```

### 3. Target an Already Running Instance

Note: Crash/restart recovery cases (`T1-F02-04`, `T1-F04-02`, `T1-F05-04`, `T2-F05-05`, `T2-F16-03`, `T2-F20-04`, `T2-F21-02`, `T3-P05`, `T3-P17`, `T3-P21`, `T4-SCEN-03`, `T4-SCEN-05`, `T4-SCEN-13`) require `--spawn-backend` and intentionally report `UNIMPL` (`requires --spawn-backend mode`) under `--target-url`.

```sh
python3 scripts/e2e/plugin_suite/run.py --target-url http://127.0.0.1:18765 --tier 1
```

### CLI Options

| Flag | Description |
|---|---|
| `--self-test` | Run against the in-process `SelfTestStubServer` and validate registry thresholds |
| `--spawn-backend` | Launch an isolated `crawler-media serve` subprocess |
| `--target-url <url>` | Connect to an already running backend instance |
| `--tier <1\|2\|3\|4>` | Run specific tier(s); repeatable |
| `--all` | Run all tiers (`1..4`) |
| `--feature <1..26>` | Filter cases by Feature Inventory ID (`1..26`) |
| `--bin <path>` | Override `crawler-media` binary path |
| `--web-dir <path>` | Set `CRAWLER_MEDIA_UI` when spawning backend |
| `--admin-password <pw>` | Override admin password for login (default: `e2e-admin-password-secret`) |
| `--cli-token <tok>` | Override `CRAWLER_MEDIA_TOKEN` when spawning backend (default: `e2e-cli-token-secret`) |
| `--json-report <path>` | Write structured JSON report to `<path>` |
| `--keep-tmp` | Keep `/tmp/crawler-e2e-*` directories after completion |

## Directory Layout

- `lib/`: `client.py`, `fixtures.py`, `instance.py`, `mocks.py`, `stub_server.py`.
- `tiers/`:
  - `_helpers.py`: shared opaque-box helpers (`obtain_reauth`, `pin_trust_key`, `prepare_and_install_plugin`, `run_plugin_cli`, etc.).
  - `tier1_features.py`: Tier 1 aggregator (`130` feature cases + `2` smoke = `132` across `t1_p1c.py`, `t1_p2.py`, `t1_p3_p4.py`, `t1_p5.py`, `t1_p6.py`).
  - `tier2_boundaries.py`: Tier 2 aggregator (`130` boundary cases + `2` smoke = `132` across `t2_p1c.py`, `t2_p2.py`, `t2_p3_p4.py`, `t2_p5.py`, `t2_p6.py`).
  - `tier3_combinations.py`: Tier 3 pairwise cross-feature combination tests (`29` pairwise cases `T3-P01`–`T3-P29` + `1` smoke `T3-INFRA-01` = `30`, covering Features `#1`–`#26`).
  - `tier4_workloads.py`: Tier 4 real-world end-to-end application workloads (`13` scenarios `T4-SCEN-01`–`T4-SCEN-13` + `1` smoke `T4-INFRA-01` = `14`, totaling `308` cases across Tiers 1–4).
- `tests/`: unit, AST, threshold, and fault-injection self-tests (`test_suite_self.py`).

## Unit Self-Tests

```sh
python3 -m unittest discover -s scripts/e2e/plugin_suite/tests -p 'test_*.py' -v
```
