#!/usr/bin/env python3
"""Opaque-box E2E test runner for the crawler-media plugin system (Python 3.9+ stdlib)."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import sys
import time
import traceback
from typing import Any, Dict, List, Optional, Sequence

if not __package__:
    _REPO_ROOT = str(Path(__file__).resolve().parents[3])
    if _REPO_ROOT not in sys.path:
        sys.path.insert(0, _REPO_ROOT)

from scripts.e2e.plugin_suite.lib.client import ApiClient, ApiError
from scripts.e2e.plugin_suite.lib.instance import InstanceSupervisor
from scripts.e2e.plugin_suite.lib.stub_server import SelfTestStubServer
from scripts.e2e.plugin_suite.tiers import (
    SuiteRegistry,
    TestCase,
    TestContext,
    UnimplementedFeatureError,
    build_default_registry,
    validate_registry_thresholds,
)


def parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Opaque-box E2E runner for crawler-media plugin system"
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run against the in-process stub server to validate runner, mocks, and fixtures",
    )
    parser.add_argument(
        "--tier",
        action="append",
        type=int,
        choices=[1, 2, 3, 4],
        help="Run specific tier(s) (1..4); may be specified multiple times",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="Run all tiers (1..4)",
    )
    parser.add_argument(
        "--feature",
        type=int,
        help="Filter test cases by Feature Inventory ID (1..26)",
    )
    parser.add_argument(
        "--target-url",
        help="Base URL of an already running crawler-media instance (e.g. http://127.0.0.1:18765)",
    )
    parser.add_argument(
        "--spawn-backend",
        action="store_true",
        help="Spawn an isolated crawler-media serve instance under /tmp/crawler-e2e-*",
    )
    parser.add_argument(
        "--bin",
        type=Path,
        help="Path to crawler-media binary (default: target/debug/crawler-media)",
    )
    parser.add_argument(
        "--web-dir",
        type=Path,
        help="Optional path to built web/dist directory for CRAWLER_MEDIA_UI",
    )
    parser.add_argument(
        "--admin-password",
        default="e2e-admin-password-secret",
        help="Admin password for login (default: e2e-admin-password-secret)",
    )
    parser.add_argument(
        "--cli-token",
        default="e2e-cli-token-secret",
        help="CRAWLER_MEDIA_TOKEN when spawning backend (default: e2e-cli-token-secret)",
    )
    parser.add_argument(
        "--json-report",
        type=Path,
        help="Optional path to write structured JSON test report",
    )
    parser.add_argument(
        "--keep-tmp",
        action="store_true",
        help="Preserve /tmp/crawler-e2e-* temporary directories after run",
    )
    args = parser.parse_args(argv)

    if args.feature is not None and not (1 <= args.feature <= 26):
        parser.error("--feature must be between 1 and 26")

    modes = sum(
        [
            bool(args.self_test),
            bool(args.target_url),
            bool(args.spawn_backend),
        ]
    )
    if modes == 0:
        parser.error("Specify one execution target: --self-test, --spawn-backend, or --target-url <url>")
    if modes > 1:
        parser.error("--self-test, --spawn-backend, and --target-url are mutually exclusive")
    if args.all and args.tier:
        parser.error("--all and --tier are mutually exclusive")

    return args


def _execute_case(case: TestCase, ctx_factory: Any) -> Dict[str, Any]:
    started = time.perf_counter()
    status = "pass"
    unimplemented = False
    error_msg: Optional[str] = None
    tb_text: Optional[str] = None

    with ctx_factory() as ctx:
        try:
            case.fn(ctx)
        except UnimplementedFeatureError as exc:
            status = "fail"
            unimplemented = True
            error_msg = str(exc)
        except ApiError as exc:
            status = "fail"
            if exc.status in (404, 405, 501):
                unimplemented = True
                error_msg = f"UNIMPLEMENTED ({exc})"
            else:
                error_msg = str(exc)
            tb_text = traceback.format_exc()
        except Exception as exc:
            status = "fail"
            msg = str(exc)
            if "UNIMPLEMENTED" in msg:
                unimplemented = True
            error_msg = f"{type(exc).__name__}: {msg}" if msg else type(exc).__name__
            tb_text = traceback.format_exc()

    elapsed_ms = round((time.perf_counter() - started) * 1000.0, 2)
    return {
        "id": case.id,
        "tier": case.tier,
        "feature_id": case.feature_id,
        "name": case.name,
        "status": status,
        "unimplemented": unimplemented,
        "duration_ms": elapsed_ms,
        "error": error_msg,
        "traceback": tb_text,
    }


def run_suite(
    args: argparse.Namespace,
    registry: Optional[SuiteRegistry] = None,
) -> Dict[str, Any]:
    reg = registry or build_default_registry()
    if args.self_test and registry is None:
        validate_registry_thresholds(reg)
    tiers: Optional[List[int]] = None
    if args.all:
        tiers = [1, 2, 3, 4]
    elif args.tier:
        tiers = sorted(set(args.tier))
    elif args.self_test:
        tiers = [1, 2, 3, 4]
    else:
        tiers = [1, 2, 3, 4]

    cases = reg.filter(
        tiers=tiers,
        feature_id=args.feature,
        self_test=bool(args.self_test),
    )

    started_at = datetime.now(timezone.utc).isoformat()
    results: List[Dict[str, Any]] = []

    stub: Optional[SelfTestStubServer] = None
    supervisor: Optional[InstanceSupervisor] = None
    admin_client: Optional[ApiClient] = None

    try:
        if args.self_test:
            mode = "self-test"
            stub = SelfTestStubServer(
                admin_password=args.admin_password,
                cli_token=args.cli_token,
            )
            base_url = stub.start()
        elif args.spawn_backend:
            mode = "spawn-backend"
            supervisor = InstanceSupervisor(
                bin_path=args.bin,
                web_dir=args.web_dir,
                cli_token=args.cli_token,
                admin_password=args.admin_password,
                keep_tmp=args.keep_tmp,
            )
            base_url = supervisor.start()
        else:
            mode = "target-url"
            base_url = str(args.target_url).rstrip("/")

        def live_url() -> str:
            if supervisor is not None and supervisor.port is not None:
                return supervisor.base_url.rstrip("/")
            return base_url

        admin_client = ApiClient(live_url())
        admin_client.login("admin", args.admin_password)
        admin_client.close()

        def make_ctx() -> TestContext:
            current = live_url()
            return TestContext(
                base_url=current,
                client=ApiClient(
                    current,
                    token=admin_client.token,
                    session_cookie=admin_client.session_cookie,
                ),
                admin_password=args.admin_password,
                cli_token=args.cli_token,
                self_test=bool(args.self_test),
                supervisor=supervisor,
                bin_path=args.bin,
                web_dir=args.web_dir,
                keep_tmp=args.keep_tmp,
            )

        for idx, tc in enumerate(cases):
            if idx > 0:
                if supervisor is not None and (
                    supervisor.proc is None or supervisor.proc.poll() is not None
                ):
                    supervisor.start()
                current = live_url()
                if admin_client.base_url != current:
                    admin_client.close()
                    admin_client = ApiClient(
                        current,
                        token=admin_client.token,
                        session_cookie=admin_client.session_cookie,
                    )
                try:
                    admin_client.me()
                except ApiError as exc:
                    if exc.status == 401:
                        admin_client.login("admin", args.admin_password)
                    else:
                        raise
                finally:
                    admin_client.close()
            res = _execute_case(tc, make_ctx)
            results.append(res)
            tag = "PASS" if res["status"] == "pass" else ("UNIMPL" if res["unimplemented"] else "FAIL")
            print(
                f"[{tag}] {res['id']} (T{res['tier']}/F{res['feature_id']:02d}) "
                f"{res['name']} ({res['duration_ms']}ms)"
            )
            if res["status"] != "pass" and res["error"]:
                print(f"       -> {res['error']}")
    finally:
        if admin_client is not None:
            admin_client.close()
        if supervisor is not None:
            supervisor.cleanup()
        if stub is not None:
            stub.stop()

    passed = sum(1 for r in results if r["status"] == "pass")
    failed = sum(1 for r in results if r["status"] != "pass")
    unimpl = sum(1 for r in results if r["unimplemented"])

    report = {
        "started_at": started_at,
        "finished_at": datetime.now(timezone.utc).isoformat(),
        "mode": mode,
        "tiers": tiers,
        "feature_filter": args.feature,
        "registry_counts": reg.counts_by_tier(),
        "summary": {
            "total": len(results),
            "passed": passed,
            "failed": failed,
            "unimplemented": unimpl,
        },
        "results": results,
    }

    if args.json_report:
        args.json_report.parent.mkdir(parents=True, exist_ok=True)
        args.json_report.write_text(
            json.dumps(report, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )

    print(
        f"\nSummary ({mode}): total={len(results)} passed={passed} "
        f"failed={failed} unimplemented={unimpl}"
    )
    return report


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(argv)
    report = run_suite(args)
    summary = report["summary"]
    if summary["total"] == 0:
        print("ERROR: No test cases matched the selected filters", file=sys.stderr)
        return 2
    return 0 if summary["failed"] == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
