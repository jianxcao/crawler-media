"""Tier 1 Happy-Path Tests for Features #9-#13 (P2 Net Guard, Worlds, Browser & Check-in)."""
from __future__ import annotations

import concurrent.futures
import time

from ..lib.fixtures import (
    build_cmplugin,
    generate_minisign_keypair,
    load_fixture_wasm,
    sign_cmplugin_sha256,
)
from . import SuiteRegistry, TestContext
from ._helpers import (
    disable_plugin,
    enable_plugin,
    ensure_downloader,
    ensure_site,
    extract_prep_grants,
    grant_site_browser_permission,
    poll_operation,
    put_browser_settings,
    setup_signed_plugin_ready,
    uniq,
    upload_cmplugin,
)


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #9: Atomic indexer::RegistrySnapshot & YAML Namespace
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F09-01",
        tier=1,
        feature_id=9,
        name="Signed yaml-only plugin registers plugin:<id>/<provider> in /sites/catalog",
        self_test=False,
    )
    def test_f09_yaml_catalog(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('yamlcat')}"
        setup_signed_plugin_ready(
            ctx, plugin_id=pid, kind="yaml-only", site_id="e2e-demo", enable=True
        )
        catalog = ctx.call_phase1("GET", "/api/v1/sites/catalog")
        expected_ref = f"plugin:{pid}/e2e-demo"
        matches = [c for c in catalog if c.get("profile_id") == expected_ref]
        assert len(matches) == 1, f"Missing {expected_ref} in catalog: {catalog!r}"
        assert matches[0].get("source") in ("plugin", "plugin-yaml", "plugin-wasm")

    @registry.case(
        id="T1-F09-02",
        tier=1,
        feature_id=9,
        name="Site using plugin:<id>/<provider> searches torrents on MockPtSiteServer",
        self_test=False,
    )
    def test_f09_yaml_site_search(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('yamlsrc')}"
        setup_signed_plugin_ready(
            ctx, plugin_id=pid, kind="yaml-only", site_id="e2e-demo", enable=True
        )
        ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/e2e-demo")
        res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
        items = res.get("items") if isinstance(res, dict) else res
        assert items and any("E2E.Demo.Release" in str(it) for it in items)
        assert len(pt.request_log) >= 1

    @registry.case(
        id="T1-F09-03",
        tier=1,
        feature_id=9,
        name="GET /sites/catalog dynamically reflects built-in and indexer-plugin capabilities",
        self_test=False,
    )
    def test_f09_dynamic_catalog(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('idxcat')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="indexer", enable=True)
        catalog = ctx.call_phase1("GET", "/api/v1/sites/catalog")
        assert any(c.get("profile_id") == "mteam" for c in catalog)
        assert any(c.get("profile_id") == f"plugin:{pid}/fixture" for c in catalog)

    @registry.case(
        id="T1-F09-04",
        tier=1,
        feature_id=9,
        name="Upgrading YAML plugin atomically updates /sites/catalog without tearing configured sites",
        self_test=False,
    )
    def test_f09_upgrade_snapshot(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('yamlup')}"
        pkg1, kp, p1 = setup_signed_plugin_ready(
            ctx, plugin_id=pid, version="1.0.0", kind="yaml-only", site_id="e2e-demo", enable=True
        )
        site = ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/e2e-demo")
        p1 = disable_plugin(ctx, pid)
        pkg2 = build_cmplugin(plugin_id=pid, version="1.1.0", kind="yaml-only", site_id="e2e-demo")
        sig2 = sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sig2)
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"upload_id": up2["upload_id"], "state_policy": "preserve", "target_config": {}},
            if_match=str(p1["revision"]),
            idempotency_key=uniq("prep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        up_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrade",
            json_body={
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": extract_prep_grants(prep),
                "state_policy": "preserve",
                "target_config_digest": prep.get("target_config_digest", ""),
            },
            if_match=str(p1["revision"]),
            idempotency_key=uniq("up"),
        )
        assert poll_operation(ctx, up_op["operation_id"])["state"] == "succeeded"
        enable_plugin(ctx, pid)
        catalog = ctx.call_phase1("GET", "/api/v1/sites/catalog")
        assert any(c.get("profile_id") == f"plugin:{pid}/e2e-demo" for c in catalog)
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert isinstance(status, dict)
        search_res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
        assert "E2E.Demo.Release" in str(search_res)

    @registry.case(
        id="T1-F09-05",
        tier=1,
        feature_id=9,
        name="Concurrent /search/torrents during catalog swap uses consistent RegistrySnapshot",
        self_test=False,
    )
    def test_f09_concurrent_snapshot(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid_a = f"org.example.{uniq('conca')}"
        pid_b = f"org.example.{uniq('concb')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid_a, kind="yaml-only", site_id="e2e-demo", enable=True)
        ensure_site(ctx, pt.url, profile_id=f"plugin:{pid_a}/e2e-demo")

        def run_search() -> int:
            res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
            items = res.get("items") if isinstance(res, dict) else res
            return 1 if items and "E2E.Demo.Release" in str(items) else 0

        with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
            swap_fut = pool.submit(
                setup_signed_plugin_ready,
                ctx,
                plugin_id=pid_b,
                kind="yaml-only",
                site_id="e2e-demo",
                enable=True,
            )
            futs = [pool.submit(run_search) for _ in range(4)]
            swap_fut.result()
            total = sum(f.result() for f in futs)
        assert total == 4
        catalog = ctx.call_phase1("GET", "/api/v1/sites/catalog")
        profiles = {c.get("profile_id") for c in catalog}
        assert f"plugin:{pid_a}/e2e-demo" in profiles
        assert f"plugin:{pid_b}/e2e-demo" in profiles

    # -----------------------------------------------------------------------
    # Feature #10: crates/net-guard Outbound Policy & CredentialSlot
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F10-01",
        tier=1,
        feature_id=10,
        name="Authorized indexer-plugin site-relative fetch sends host CredentialSlot without leaking Set-Cookie",
        self_test=False,
    )
    def test_f10_credential_slot(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('ng1')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="indexer", enable=True)
        ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/fixture", cookie="uid=42; pass=valid")
        res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=site-auth-http")
        items = res.get("items") if isinstance(res, dict) else res
        assert items and len(items) >= 1
        assert "uid=42" not in str(res) and "pass=valid" not in str(res)
        assert any("uid=42" in r["headers"].get("cookie", "") for r in pt.request_log)

    @registry.case(
        id="T1-F10-02",
        tier=1,
        feature_id=10,
        name="Authorized http.fetch-many executes bounded batch within call budget",
        self_test=False,
    )
    def test_f10_fetch_many(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('ngmany')}"
        setup_signed_plugin_ready(
            ctx,
            plugin_id=pid,
            kind="indexer",
            http_rules=[{"origin": "https://tracker.invalid", "path_prefix": "/", "methods": ["GET"]}],
            enable=True,
        )
        site = ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/fixture")
        for kw in ("fetch-many-empty", "fetch-many-too-many", "fetch-many", "http-fetch"):
            res = ctx.call_phase1("GET", f"/api/v1/search/torrents?keyword={kw}")
            assert isinstance(res, dict) and isinstance(res.get("sites"), list), f"Expected site status for {kw}: {res!r}"
            assert res["sites"][0].get("error"), f"Expected bounded/network rejection for {kw}: {res!r}"
        auth_res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=site-auth-http")
        assert "auth:" in str(auth_res), f"Expected authenticated site-auth-http search hit, got {auth_res!r}"
        ver = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
        assert isinstance(ver, dict) and ver.get("ok") is True
        assert len(pt.request_log) >= 2
        assert any("uid=42" in r["headers"].get("cookie", "") for r in pt.request_log)

    @registry.case(
        id="T1-F10-03",
        tier=1,
        feature_id=10,
        name="Same-origin 3-hop redirect chain preserves CredentialSlot and succeeds",
        self_test=False,
    )
    def test_f10_same_origin_redirects(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "site_id": site["id"],
                "download_url": f"{pt.url}/redirect/3",
                "title": "Redirected Release",
            },
            idempotency_key=uniq("subredir"),
        )
        if isinstance(sub, dict) and sub.get("operation_id"):
            assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
        assert len(qb.torrents) == 1
        paths = [r["path"] for r in pt.request_log]
        for hop in ("/redirect/3", "/redirect/2", "/redirect/1", "/download.torrent"):
            assert hop in paths, f"Missing hop {hop} in {paths!r}"
        assert all("uid=42" in r["headers"].get("cookie", "") for r in pt.request_log)

    @registry.case(
        id="T1-F10-04",
        tier=1,
        feature_id=10,
        name="Authorized public-url matching manifest [[http]] succeeds within 5 MiB cap",
        self_test=False,
    )
    def test_f10_manifest_http_rule(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('ngpub')}"
        setup_signed_plugin_ready(
            ctx,
            plugin_id=pid,
            kind="hook",
            hooks=[{"id": "site.checkin.after", "priority": 200}],
            hook_permissions=["hooks:site.checkin.after", "events:site.checkin.after", "http", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        site = ensure_site(ctx, pt.url, profile_id="demo", cookie="uid=42; pass=valid")
        res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] == "succeeded"
        deadline = time.monotonic() + 8.0
        while time.monotonic() < deadline and not pt.webhook_events:
            time.sleep(0.05)
        assert len(pt.webhook_events) >= 1, f"Expected manifest [[http]] webhook delivery, got {pt.request_log!r}"
        wh_reqs = [r for r in pt.request_log if r["path"] == "/webhook/events"]
        assert len(wh_reqs) >= 1
        assert wh_reqs[-1]["headers"].get("cookie", "") == ""

    @registry.case(
        id="T1-F10-05",
        tier=1,
        feature_id=10,
        name=".torrent download under 10 MiB cap delivers exact verified bytes to MockQbitServer",
        self_test=False,
    )
    def test_f10_torrent_under_cap(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Under Cap Torrent",
            },
            idempotency_key=uniq("under10m"),
        )
        if isinstance(sub, dict) and sub.get("operation_id"):
            assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
        assert pt.default_torrent.info_hash in qb.torrents
        assert (
            qb.torrents[pt.default_torrent.info_hash]["size"]
            == pt.default_torrent.verified_total_bytes
        )

    # -----------------------------------------------------------------------
    # Feature #11: indexer-plugin & maintenance-plugin Worlds + HTTP Imports
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F11-01",
        tier=1,
        feature_id=11,
        name="Enabled indexer-plugin executes indexer.search via GET /search/torrents",
        self_test=False,
    )
    def test_f11_indexer_search(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('idxp')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="indexer", enable=True)
        ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/fixture")
        res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=site-auth-http")
        items = res.get("items") if isinstance(res, dict) else res
        assert items and any("E2E.Demo.Release" in str(it) for it in items)
        assert len(pt.request_log) >= 1

    @registry.case(
        id="T1-F11-02",
        tier=1,
        feature_id=11,
        name="indexer-plugin resolves download-ref using host CredentialSlot",
        self_test=False,
    )
    def test_f11_resolve_download(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit()
        pid = f"org.example.{uniq('idxdl')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="indexer", enable=True)
        site = ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/fixture")
        dl = ensure_downloader(ctx, qb.url)
        search_res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=site-auth-http")
        items = search_res.get("items") if isinstance(search_res, dict) else search_res
        assert isinstance(items, list) and len(items) >= 1, f"Expected search result, got {search_res!r}"
        dl_ref = items[0]["enclosure"]
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "site_id": site["id"],
                "download_url": dl_ref,
                "title": "Indexed Download",
            },
            idempotency_key=uniq("idxdl"),
        )
        if isinstance(sub, dict) and sub.get("operation_id"):
            assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
        assert len(qb.torrents) == 1
        assert any("uid=42" in r["headers"].get("cookie", "") for r in pt.request_log)

    @registry.case(
        id="T1-F11-03",
        tier=1,
        feature_id=11,
        name="Signed maintenance-plugin executes verify-session and check-in via /plugin-actions",
        self_test=False,
    )
    def test_f11_maintenance_actions(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('maintp')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="maintenance", enable=True)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        prov_id = f"plugin:{pid}/e2e-maint"
        v_act = ctx.call_phase1(
            "POST",
            f"/api/v1/sites/{site['id']}/plugin-actions",
            json_body={"action": "verify-session", "provider_id": prov_id},
            if_match=str(site["revision"]),
            idempotency_key=uniq("maintver"),
        )
        if isinstance(v_act, dict) and v_act.get("operation_id"):
            assert poll_operation(ctx, v_act["operation_id"])["state"] == "succeeded"
        st_before_chk = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        act = ctx.call_phase1(
            "POST",
            f"/api/v1/sites/{site['id']}/plugin-actions",
            json_body={"action": "check-in", "provider_id": prov_id},
            if_match=str(st_before_chk["revision"]),
            idempotency_key=uniq("maintact"),
        )
        if isinstance(act, dict) and act.get("operation_id"):
            assert poll_operation(ctx, act["operation_id"])["state"] == "succeeded"
        st = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        receipts = st.get("receipts") or []
        for expected_action in ("verify-session", "check-in"):
            assert any(
                isinstance(r, dict)
                and r.get("action") == expected_action
                and r.get("plugin_id") == pid
                and r.get("provider_id") == prov_id
                and r.get("state") == "completed"
                for r in receipts
            ), f"Expected completed {expected_action} receipt for {prov_id}, got {st!r}"
        st_mid = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        act2 = ctx.call_phase1(
            "POST",
            f"/api/v1/sites/{site['id']}/plugin-actions",
            json_body={"action": "verify-session", "provider_id": prov_id},
            if_match=str(st_mid["revision"]),
            idempotency_key=uniq("maintver"),
        )
        if isinstance(act2, dict) and act2.get("operation_id"):
            assert poll_operation(ctx, act2["operation_id"])["state"] == "succeeded"
        st2 = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        receipts2 = [
            r
            for r in (st2.get("receipts") or [])
            if isinstance(r, dict) and r.get("plugin_id") == pid and r.get("provider_id") == prov_id and r.get("state") == "completed"
        ]
        assert len(receipts2) >= 2, f"Expected >=2 completed receipts for {prov_id}, got {st2!r}"
        ver = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
        assert isinstance(ver, dict) and ver.get("ok") is True
        assert len(pt.request_log) == 0, f"Expected same-window verify dedup after plugin-actions verify-session, got {pt.request_log!r}"

    @registry.case(
        id="T1-F11-04",
        tier=1,
        feature_id=11,
        name="SiteView and plugin-status never expose raw cookie, api_key, or cdp_url",
        self_test=False,
    )
    def test_f11_site_view_redaction(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=supersecretcookie")
        pid = f"org.example.{uniq('redsite')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="maintenance", enable=True)
        put_browser_settings(ctx, True, "http://127.0.0.1:9222/secret-cdp")
        site = ensure_site(ctx, pt.url, profile_id="pterclub", cookie="uid=42; pass=supersecretcookie")
        sites = ctx.call_phase1("GET", "/api/v1/sites")
        assert isinstance(sites, list) and any(s.get("id") == site["id"] for s in sites), (
            f"Expected created site {site['id']} in GET /api/v1/sites, got {sites!r}"
        )
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert "supersecretcookie" not in str(sites)
        assert "supersecretcookie" not in str(status)
        assert "secret-cdp" not in str(status) and "cdp_url" not in str(status)

    @registry.case(
        id="T1-F11-05",
        tier=1,
        feature_id=11,
        name="Same-domain Set-Cookie during maintenance updates site cookie cleanly via CAS",
        self_test=False,
    )
    def test_f11_cookie_refresh_cas(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.checkin_rotate_cookie = "pass=rotated_v2"
        site = ensure_site(ctx, pt.url, profile_id="pterclub", cookie="uid=42; pass=valid")
        res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] == "succeeded"
        pt.expected_cookie = "uid=42; pass=rotated_v2"
        ver = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
        assert ver.get("ok") is True, f"Rotated cookie verification failed: {ver!r}"
        assert "pass=rotated_v2" in pt.request_log[-1]["headers"].get("cookie", "")

    # -----------------------------------------------------------------------
    # Feature #12: Narrow BrowserSession Port (Built-in & Admin-Trusted Only)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F12-01",
        tier=1,
        feature_id=12,
        name="GET /plugins/:id/browser-availability returns ready when triple gate satisfied without leaking cdp_url",
        self_test=False,
    )
    def test_f12_browser_availability_ready(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        cdp = ctx.new_cdp()
        put_browser_settings(ctx, True, cdp.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ctx.call_phase1(
            "PATCH",
            f"/api/v1/sites/{site['id']}",
            json_body={"maintenance_via": "browser"},
        )
        grant_site_browser_permission(ctx, "builtin.nexusphp-maintenance", site["id"], "browser:maintenance")
        avail = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/builtin.nexusphp-maintenance/browser-availability?site_id={site['id']}&purpose=maintenance",
        )
        assert avail.get("status") == "ready", f"Expected ready, got {avail!r}"
        assert avail.get("missing_requirements") == []
        assert cdp.url not in str(avail) and "ws://" not in str(avail)

    @registry.case(
        id="T1-F12-02",
        tier=1,
        feature_id=12,
        name="POST /plugins/:id/browser-checks returns 202 operation_id and reaches succeeded",
        self_test=False,
    )
    def test_f12_browser_checks_op(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        cdp = ctx.new_cdp()
        put_browser_settings(ctx, True, cdp.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ctx.call_phase1(
            "PATCH",
            f"/api/v1/sites/{site['id']}",
            json_body={"maintenance_via": "browser"},
        )
        grant_site_browser_permission(ctx, "builtin.nexusphp-maintenance", site["id"], "browser:maintenance")
        p_chk = ctx.call_phase1("GET", "/api/v1/plugins/builtin.nexusphp-maintenance")
        op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/builtin.nexusphp-maintenance/browser-checks",
            json_body={"site_id": site["id"], "purpose": "maintenance"},
            if_match=str(p_chk["revision"]),
            idempotency_key=uniq("bchk"),
        )
        done = poll_operation(ctx, op["operation_id"])
        assert done["state"] == "succeeded", f"Expected browser-check succeeded, got {done!r}"
        assert len(cdp.rpc_log) >= 1, f"Expected CDP RPC calls, got {cdp.rpc_log!r}"
        assert len(cdp.disposed_contexts) >= 1, f"Expected disposed CDP context, got {cdp.disposed_contexts!r}"
        assert len(cdp.active_contexts) == 0, f"Expected all CDP contexts disposed, active={cdp.active_contexts!r}"

    @registry.case(
        id="T1-F12-03",
        tier=1,
        feature_id=12,
        name="Built-in maintenance plugin uses MockObscuraCdpServer and disposes context on completion",
        self_test=False,
    )
    def test_f12_browser_maintenance_disposes(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        cdp = ctx.new_cdp()
        put_browser_settings(ctx, True, cdp.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ctx.call_phase1(
            "PATCH",
            f"/api/v1/sites/{site['id']}",
            json_body={"maintenance_via": "browser"},
        )
        grant_site_browser_permission(ctx, "builtin.nexusphp-maintenance", site["id"], "browser:maintenance")
        res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] == "succeeded"
        assert len(cdp.disposed_contexts) >= 1
        assert len(cdp.navigated_urls) >= 1
        assert len(cdp.active_contexts) == 0
        assert pt.checkin_post_count == 0

    @registry.case(
        id="T1-F12-04",
        tier=1,
        feature_id=12,
        name="Built-in/trusted browser search disposes context and leaves stored site cookie unchanged",
        self_test=False,
    )
    def test_f12_browser_search_stateless_cookies(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        cdp = ctx.new_cdp(
            html_content=(
                "<html><body><table>"
                "<tr class='torrent'>"
                "<td class='category'>Movie</td>"
                f"<td><a class='name' title='E2E.Demo.Release.1080p.WEB-DL' href='details.php?id=101'>E2E.Demo.Release.1080p.WEB-DL</a>"
                f"<a class='download' href='{pt.url}/torrents/101.torrent'>DL</a></td>"
                "<td class='upload_time'>2026-11-20 12:00:00</td>"
                "<td class='size'>1.00 GiB</td>"
                "<td class='seeders'>24</td>"
                "<td class='leechers'>2</td>"
                "<td class='snatched'>15</td>"
                "</tr>"
                "</table><a href='logout.php'>Logout</a></body></html>"
            )
        )
        put_browser_settings(ctx, True, cdp.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ctx.call_phase1(
            "PATCH",
            f"/api/v1/sites/{site['id']}",
            json_body={"browser_search": True},
        )
        grant_site_browser_permission(ctx, "builtin.nexusphp-maintenance", site["id"], "browser:maintenance")
        b_settings = ctx.call_phase1("GET", "/api/v1/settings/browser")
        assert int(b_settings.get("browser_sites", 0)) >= 1, f"Expected browser_sites >= 1, got {b_settings!r}"
        assert (b_settings.get("capabilities") or {}).get("search") == "http_only", f"Unexpected caps: {b_settings!r}"
        avail_search = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/builtin.nexusphp-maintenance/browser-availability?site_id={site['id']}&purpose=search",
        )
        assert avail_search.get("status") != "ready", f"Search purpose must not report ready: {avail_search!r}"
        assert cdp.url not in str(avail_search) and "ws://" not in str(avail_search)
        p_chk = ctx.call_phase1("GET", "/api/v1/plugins/builtin.nexusphp-maintenance")
        b_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/builtin.nexusphp-maintenance/browser-checks",
            json_body={"site_id": site["id"], "purpose": "maintenance"},
            if_match=str(p_chk["revision"]),
            idempotency_key=uniq("t1f1204"),
        )
        assert poll_operation(ctx, b_op["operation_id"])["state"] == "succeeded"
        res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
        assert "E2E.Demo.Release" in str(res)
        assert len(cdp.disposed_contexts) >= 1
        assert len(cdp.active_contexts) == 0
        ver = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
        assert ver.get("ok") is True
        assert "uid=42; pass=valid" in pt.request_log[-1]["headers"].get("cookie", "")

    @registry.case(
        id="T1-F12-05",
        tier=1,
        feature_id=12,
        name="Concurrent BrowserSession calls across two sites isolate contexts and dispose cleanly",
        self_test=False,
    )
    def test_f12_concurrent_browser_contexts(ctx: TestContext) -> None:
        pt1 = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt2 = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        cdp = ctx.new_cdp()
        put_browser_settings(ctx, True, cdp.url)
        s1 = ensure_site(ctx, pt1.url, profile_id="pterclub")
        s2 = ensure_site(ctx, pt2.url, profile_id="pterclub")
        for s in (s1, s2):
            ctx.call_phase1("PATCH", f"/api/v1/sites/{s['id']}", json_body={"maintenance_via": "browser"})
            grant_site_browser_permission(ctx, "builtin.nexusphp-maintenance", s["id"], "browser:maintenance")
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            f1 = pool.submit(ctx.call_phase1, "POST", f"/api/v1/sites/{s1['id']}/check-in", json_body={})
            f2 = pool.submit(ctx.call_phase1, "POST", f"/api/v1/sites/{s2['id']}/check-in", json_body={})
            r1 = f1.result()
            r2 = f2.result()
        for r in (r1, r2):
            if isinstance(r, dict) and r.get("operation_id"):
                assert poll_operation(ctx, r["operation_id"])["state"] == "succeeded"
        assert len(cdp.disposed_contexts) >= 2
        assert len(cdp.navigated_urls) >= 2
        assert len(cdp.active_contexts) == 0
        assert pt1.checkin_post_count == 0 and pt2.checkin_post_count == 0

    # -----------------------------------------------------------------------
    # Feature #13: site_maintenance_receipts & First-Party Check-in Migration
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F13-01",
        tier=1,
        feature_id=13,
        name="builtin.nexusphp-maintenance executes check-in once and records completed receipt",
        self_test=False,
    )
    def test_f13_checkin_receipt(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins/builtin.nexusphp-maintenance")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert "completed" in str(status).lower()

    @registry.case(
        id="T1-F13-02",
        tier=1,
        feature_id=13,
        name="Re-running check-in in same window returns already-completed with 0 extra POSTs",
        self_test=False,
    )
    def test_f13_window_dedup(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins/builtin.nexusphp-maintenance")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        res1 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(res1, dict) and res1.get("operation_id"):
            assert poll_operation(ctx, res1["operation_id"])["state"] == "succeeded"
        res2 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(res2, dict) and res2.get("operation_id"):
            res2 = poll_operation(ctx, res2["operation_id"])
        assert pt.checkin_post_count == 1
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert "already" in str(res2).lower() or "already" in str(status).lower()

    @registry.case(
        id="T1-F13-03",
        tier=1,
        feature_id=13,
        name="POST /sites/:id/verify uses verify-session capability instead of blank search",
        self_test=False,
    )
    def test_f13_verify_session(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins/builtin.nexusphp-maintenance")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
        assert isinstance(res, dict) and res.get("ok") is True
        assert len(pt.request_log) >= 1
        assert not any("/torrents.php?search=%20" in r["path"] for r in pt.request_log)

    @registry.case(
        id="T1-F13-04",
        tier=1,
        feature_id=13,
        name="Framework::Api site with check-in capability is not skipped on jobs/tick",
        self_test=False,
    )
    def test_f13_api_site_not_skipped(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_api_key="mteam-key")
        pid = f"org.example.{uniq('apimaint')}"
        setup_signed_plugin_ready(
            ctx,
            plugin_id=pid,
            kind="yaml-only",
            site_id="e2e-api",
            site_framework="api",
            credential_binding="invocation-site",
            extra_files={"components/maintenance.wasm": load_fixture_wasm("maintenance")},
            extra_components=[
                {
                    "id": "maintenance",
                    "entry": "components/maintenance.wasm",
                    "world": "crawler:plugin/maintenance-plugin@1.0.0",
                    "permissions": ["maintenance:provide", "http:site-auth", "kv"],
                    "maintenance": [{"id": "e2e-maint", "actions": ["verify-session", "check-in"]}],
                }
            ],
            enable=True,
        )
        site = ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/e2e-api", api_key="mteam-key")
        res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] == "succeeded"
        ctx.call_phase1("POST", "/api/v1/jobs/tick", json_body={})
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert "completed" in str(status).lower()

    @registry.case(
        id="T1-F13-05",
        tier=1,
        feature_id=13,
        name="Site already-checked-in HTML records already-completed and deduplicates",
        self_test=False,
    )
    def test_f13_already_html(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins/builtin.nexusphp-maintenance")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.checkin_mode = "already"
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        r1 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(r1, dict) and r1.get("operation_id"):
            assert poll_operation(ctx, r1["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1
        r2 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(r2, dict) and r2.get("operation_id"):
            assert poll_operation(ctx, r2["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert "already" in str(status).lower() or "completed" in str(status).lower()
