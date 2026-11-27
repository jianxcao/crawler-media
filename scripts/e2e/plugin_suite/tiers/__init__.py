"""Tier registry and shared opaque-box test context for crawler-media E2E."""
from __future__ import annotations

from contextlib import ExitStack
from dataclasses import dataclass, field
from pathlib import Path
import shutil
import tempfile
import time
from typing import Any, Callable, Dict, List, Optional, Sequence, Tuple
import uuid

from ..lib.client import ApiClient, ApiError
from ..lib.instance import InstanceSupervisor, resolve_e2e_tmp_root
from ..lib.mocks import MockObscuraCdpServer, MockPtSiteServer, MockQbitServer


class UnimplementedFeatureError(AssertionError):
    """Raised when a Phase 1 endpoint returns 404/501 on a target backend."""

    def __init__(self, endpoint: str, detail: str = "") -> None:
        msg = f"UNIMPLEMENTED Phase 1 endpoint: {endpoint}"
        if detail:
            msg = f"{msg} ({detail})"
        super().__init__(msg)
        self.endpoint = endpoint


@dataclass
class TestCase:
    """Single opaque-box E2E test case definition."""

    id: str
    tier: int
    feature_id: int
    name: str
    fn: Callable[[TestContext], None]
    self_test: bool = True
    related_features: Tuple[int, ...] = ()

    def __post_init__(self) -> None:
        if self.tier not in (1, 2, 3, 4):
            raise ValueError(f"Invalid tier {self.tier}; expected 1..4")
        if not (1 <= self.feature_id <= 26):
            raise ValueError(f"Invalid feature_id {self.feature_id}; expected 1..26")
        for fid in self.related_features:
            if not (1 <= fid <= 26):
                raise ValueError(f"Invalid related_feature {fid}; expected 1..26")


class TestContext:
    """Per-test isolated context providing ApiClient, mock server factories, and temp dir."""

    def __init__(
        self,
        base_url: str,
        client: ApiClient,
        admin_password: str,
        cli_token: str,
        self_test: bool = False,
        supervisor: Optional[InstanceSupervisor] = None,
        bin_path: Optional[Path] = None,
        web_dir: Optional[Path] = None,
        keep_tmp: bool = False,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.client = client
        self.admin_password = admin_password
        self.cli_token = cli_token
        self.self_test = self_test
        self.supervisor = supervisor
        self.bin_path = bin_path
        self.web_dir = web_dir
        self.keep_tmp = keep_tmp
        self.tmp_dir = Path(
            tempfile.mkdtemp(prefix="crawler-e2e-case-", dir=resolve_e2e_tmp_root())
        )
        self._stack = ExitStack()
        self._clients: List[ApiClient] = [client]
        self.created_site_ids: List[str] = []
        self.created_plugin_ids: List[str] = []
        self.created_downloader_ids: List[str] = []

    def new_qbit(self, **kwargs: Any) -> MockQbitServer:
        srv = MockQbitServer(**kwargs)
        self._stack.enter_context(srv)
        return srv

    def new_pt_site(self, **kwargs: Any) -> MockPtSiteServer:
        srv = MockPtSiteServer(**kwargs)
        self._stack.enter_context(srv)
        return srv

    def new_cdp(self, **kwargs: Any) -> MockObscuraCdpServer:
        srv = MockObscuraCdpServer(**kwargs)
        self._stack.enter_context(srv)
        return srv

    def new_client(self, token: Optional[str] = None, session_cookie: Optional[str] = None) -> ApiClient:
        if self.supervisor is not None and self.supervisor.port is not None:
            self.base_url = self.supervisor.base_url.rstrip("/")
        cli = ApiClient(self.base_url, token=token, session_cookie=session_cookie)
        self._clients.append(cli)
        return cli

    def restart_server(
        self,
        *,
        kill: bool = False,
        extra_env: Optional[Dict[str, str]] = None,
        reuse_port: bool = False,
        relogin: bool = True,
    ) -> str:
        if self.supervisor is None:
            raise UnimplementedFeatureError(
                "InstanceSupervisor crash/restart",
                "requires --spawn-backend mode",
            )
        if kill:
            old_port = self.supervisor.port if reuse_port else None
            self.supervisor.kill()
            new_url = self.supervisor.start(extra_env=extra_env, port=old_port)
        else:
            new_url = self.supervisor.restart(extra_env=extra_env, reuse_port=reuse_port)
        self.base_url = new_url.rstrip("/")
        self.client.close()
        self.client = ApiClient(
            self.base_url,
            token=self.client.token,
            session_cookie=self.client.session_cookie,
            timeout=self.client.timeout,
        )
        self._clients.append(self.client)
        if relogin:
            self.client.login("admin", self.admin_password)
        return self.base_url

    def call_phase1(self, method: str, path: str, **kwargs: Any) -> Any:
        """Invoke a Phase 1 endpoint, failing explicitly with UNIMPLEMENTED on 404/501."""
        try:
            return self.client.request(method, path, **kwargs)
        except ApiError as exc:
            if exc.status in (404, 405, 501):
                raise UnimplementedFeatureError(f"{method} {path}", f"HTTP {exc.status}") from exc
            raise

    def _cleanup_remote_state(self) -> None:
        for pid in reversed(self.created_plugin_ids):
            if pid.startswith("builtin."):
                continue
            try:
                cur = self.client.request("GET", f"/api/v1/plugins/{pid}")
                if isinstance(cur, dict) and cur.get("lifecycle") == "enabled":
                    self.client.request(
                        "POST",
                        f"/api/v1/plugins/{pid}/disable",
                        json_body={"reason": "e2e-cleanup"},
                        if_match=str(cur["revision"]),
                        idempotency_key=f"cln-dis-{uuid.uuid4().hex[:12]}",
                    )
                    for _ in range(40):
                        try:
                            self.client.request("POST", "/api/v1/jobs/tick", json_body={})
                        except Exception:
                            pass
                        st = self.client.request("GET", f"/api/v1/plugins/{pid}")
                        if st.get("lifecycle") in ("disabled", "installed-disabled"):
                            cur = st
                            break
                        time.sleep(0.05)
                if isinstance(cur, dict) and "revision" in cur:
                    self.client.request(
                        "POST",
                        f"/api/v1/plugins/{pid}/uninstall",
                        json_body={"remove_private_data": True},
                        if_match=str(cur["revision"]),
                        idempotency_key=f"cln-un-{uuid.uuid4().hex[:12]}",
                    )
            except Exception:
                pass
        try:
            subs = self.client.request("GET", "/api/v1/subscriptions")
            sub_items = subs.get("items") if isinstance(subs, dict) else subs
            for sub in (sub_items or []):
                if isinstance(sub, dict) and sub.get("id"):
                    try:
                        self.client.request("DELETE", f"/api/v1/subscriptions/{sub['id']}")
                    except Exception:
                        pass
        except Exception:
            pass
        for sid in reversed(self.created_site_ids):
            try:
                self.client.request("DELETE", f"/api/v1/sites/{sid}")
            except Exception:
                pass
        for dl_id in reversed(self.created_downloader_ids):
            try:
                self.client.request("DELETE", f"/api/v1/downloaders/{dl_id}")
            except Exception:
                pass
        for b_pid in ("builtin.nexusphp-maintenance", "cm.checkin", "cm.boost"):
            try:
                cur = self.client.request("GET", f"/api/v1/plugins/{b_pid}")
                if isinstance(cur, dict) and cur.get("lifecycle") in ("disabled", "installed-disabled"):
                    self.client.request(
                        "POST",
                        f"/api/v1/plugins/{b_pid}/enable",
                        json_body={},
                        if_match=str(cur["revision"]),
                        idempotency_key=f"cln-en-{uuid.uuid4().hex[:12]}",
                    )
                    for _ in range(40):
                        try:
                            self.client.request("POST", "/api/v1/jobs/tick", json_body={})
                        except Exception:
                            pass
                        st = self.client.request("GET", f"/api/v1/plugins/{b_pid}")
                        if st.get("lifecycle") == "enabled":
                            break
                        time.sleep(0.05)
            except Exception:
                pass

    def close(self) -> None:
        try:
            self._cleanup_remote_state()
            for cli in self._clients:
                try:
                    cli.close()
                except OSError:
                    pass
            self._clients.clear()
            self._stack.close()
        finally:
            if not self.keep_tmp and self.tmp_dir.exists():
                shutil.rmtree(self.tmp_dir, ignore_errors=True)

    def __enter__(self) -> TestContext:
        return self

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        self.close()


class SuiteRegistry:
    """Registry holding all Tier 1-4 test cases."""

    def __init__(self) -> None:
        self._cases: List[TestCase] = []
        self._seen_ids: set = set()

    def register(self, case: TestCase) -> TestCase:
        if case.id in self._seen_ids:
            raise ValueError(f"Duplicate test case id: {case.id!r}")
        self._seen_ids.add(case.id)
        self._cases.append(case)
        return case

    def case(
        self,
        id: str,
        tier: int,
        feature_id: int,
        name: str,
        self_test: bool = True,
        related_features: Tuple[int, ...] = (),
    ) -> Callable[[Callable[[TestContext], None]], Callable[[TestContext], None]]:
        def _decorator(fn: Callable[[TestContext], None]) -> Callable[[TestContext], None]:
            self.register(
                TestCase(
                    id=id,
                    tier=tier,
                    feature_id=feature_id,
                    name=name,
                    fn=fn,
                    self_test=self_test,
                    related_features=tuple(related_features),
                )
            )
            return fn

        return _decorator

    @property
    def cases(self) -> List[TestCase]:
        return list(self._cases)

    def filter(
        self,
        tiers: Optional[Sequence[int]] = None,
        feature_id: Optional[int] = None,
        self_test: bool = False,
    ) -> List[TestCase]:
        out: List[TestCase] = []
        tier_set = set(tiers) if tiers else None
        for tc in self._cases:
            if tier_set is not None and tc.tier not in tier_set:
                continue
            if feature_id is not None and tc.feature_id != feature_id:
                continue
            if self_test and not tc.self_test:
                continue
            out.append(tc)
        return out

    def counts_by_tier(self) -> Dict[int, int]:
        counts = {1: 0, 2: 0, 3: 0, 4: 0}
        for tc in self._cases:
            counts[tc.tier] = counts.get(tc.tier, 0) + 1
        return counts


def validate_registry_thresholds(registry: SuiteRegistry) -> Dict[str, Any]:
    """Verify suite-wide uniqueness and Tier 1-4 coverage thresholds."""
    cases = registry.cases
    ids = [c.id for c in cases]
    if len(ids) != len(set(ids)):
        raise ValueError("Duplicate test case IDs detected in registry")

    t1_per_feat: Dict[int, int] = {f: 0 for f in range(1, 27)}
    t2_per_feat: Dict[int, int] = {f: 0 for f in range(1, 27)}
    smoke_by_tier: Dict[int, int] = {1: 0, 2: 0, 3: 0, 4: 0}
    non_smoke_by_tier: Dict[int, int] = {1: 0, 2: 0, 3: 0, 4: 0}
    t3_features_covered: set = set()

    for c in cases:
        if c.self_test:
            smoke_by_tier[c.tier] = smoke_by_tier.get(c.tier, 0) + 1
        else:
            non_smoke_by_tier[c.tier] = non_smoke_by_tier.get(c.tier, 0) + 1

        if c.tier == 1:
            if not c.self_test:
                t1_per_feat[c.feature_id] += 1
            if not c.id.endswith("SMOKE-01") and not c.id.endswith("SMOKE-02") and c.self_test:
                raise ValueError(f"{c.id} must have self_test=False")
        elif c.tier == 2:
            if not c.self_test:
                t2_per_feat[c.feature_id] += 1
            if not c.id.endswith("SMOKE-01") and not c.id.endswith("SMOKE-02") and c.self_test:
                raise ValueError(f"{c.id} must have self_test=False")
        elif c.tier == 3 and not c.self_test:
            distinct = {c.feature_id, *c.related_features}
            if len(distinct) < 2:
                raise ValueError(
                    f"Tier 3 pairwise case {c.id} must declare >= 2 distinct features, got {sorted(distinct)}"
                )
            t3_features_covered.update(distinct)
        elif c.tier == 4 and not c.self_test:
            distinct = {c.feature_id, *c.related_features}
            if len(distinct) < 2:
                raise ValueError(
                    f"Tier 4 scenario {c.id} must declare >= 2 distinct features, got {sorted(distinct)}"
                )

    for fid in range(1, 27):
        if t1_per_feat[fid] < 5:
            raise ValueError(f"Feature #{fid} has < 5 Tier 1 cases: {t1_per_feat[fid]}")
        if t2_per_feat[fid] < 5:
            raise ValueError(f"Feature #{fid} has < 5 Tier 2 cases: {t2_per_feat[fid]}")
        if fid not in t3_features_covered:
            raise ValueError(f"Feature #{fid} missing from Tier 3 non-smoke pairwise coverage")

    for tier in (1, 2, 3, 4):
        if smoke_by_tier[tier] < 1:
            raise ValueError(f"Tier {tier} must contain >= 1 self_test=True smoke case")

    if non_smoke_by_tier[1] < 130:
        raise ValueError(f"Tier 1 non-smoke count < 130: {non_smoke_by_tier[1]}")
    if non_smoke_by_tier[2] < 130:
        raise ValueError(f"Tier 2 non-smoke count < 130: {non_smoke_by_tier[2]}")
    if non_smoke_by_tier[3] < 26:
        raise ValueError(f"Tier 3 non-smoke count < 26: {non_smoke_by_tier[3]}")
    if non_smoke_by_tier[4] < 13:
        raise ValueError(f"Tier 4 non-smoke count < 13: {non_smoke_by_tier[4]}")

    non_smoke_total = sum(non_smoke_by_tier.values())
    if non_smoke_total < 299:
        raise ValueError(f"Non-smoke total < 299: {non_smoke_total}")
    if len(cases) < 303:
        raise ValueError(f"Total cases < 303: {len(cases)}")

    return {
        "counts_by_tier": registry.counts_by_tier(),
        "non_smoke_by_tier": non_smoke_by_tier,
        "smoke_by_tier": smoke_by_tier,
        "non_smoke_total": non_smoke_total,
        "total": len(cases),
        "tier3_features_covered": sorted(t3_features_covered),
    }


def build_default_registry() -> SuiteRegistry:
    from . import (
        tier1_features,
        tier2_boundaries,
        tier3_combinations,
        tier4_workloads,
    )

    registry = SuiteRegistry()
    tier1_features.register_cases(registry)
    tier2_boundaries.register_cases(registry)
    tier3_combinations.register_cases(registry)
    tier4_workloads.register_cases(registry)
    return registry
