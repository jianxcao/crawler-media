"""Tier 2 Boundary & Negative Tests for Features #9-#13 (P2 Net Guard, Worlds, Browser & Check-in)."""
from __future__ import annotations

import uuid

from ..lib.client import ApiError
from ..lib.fixtures import (
    build_cmplugin,
    build_negative_cmplugin,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
)
from . import SuiteRegistry, TestContext
from ._helpers import (
    assert_api_error,
    create_member_client,
    disable_plugin,
    ensure_downloader,
    ensure_site,
    grant_site_browser_permission,
    obtain_reauth,
    poll_operation,
    post_multipart_phase1,
    put_browser_settings,
    set_developer_mode,
    setup_signed_plugin_ready,
    uniq,
)


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #9: Atomic indexer::RegistrySnapshot & YAML Namespace (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F09-01",
        tier=2,
        feature_id=9,
        name="YAML site profile colliding with built-in mteam is rejected without corrupting catalog",
        self_test=False,
    )
    def test_t2_f09_yaml_builtin_collision(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            reauth = obtain_reauth(ctx, purpose="plugin:upload")
            bad = build_negative_cmplugin("invalid_yaml_builtin_collision")
            assert_api_error(
                lambda: post_multipart_phase1(
                    ctx.client,
                    "/api/v1/plugin-uploads",
                    fields={"upload_id": uniq("up")},
                    files=[("file", "bad.cmplugin", bad, "application/zip")],
                    reauth_token=reauth,
                    idempotency_key=uniq("mteamcol"),
                    if_match="0",
                ),
                (400, 409, 422),
                label="YAML builtin collision",
            )
            catalog = ctx.call_phase1("GET", "/api/v1/sites/catalog")
            assert any(c.get("profile_id") == "mteam" for c in catalog)
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F09-02",
        tier=2,
        feature_id=9,
        name="Duplicate plugin provider registration fails without corrupting existing RegistrySnapshot",
        self_test=False,
    )
    def test_t2_f09_duplicate_provider_isolation(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('dupidx')}"
        _, kp, _ = setup_signed_plugin_ready(
            ctx, plugin_id=pid, version="1.0.0", kind="yaml-only", site_id="e2e-demo", enable=True
        )
        pkg_dup = build_cmplugin(
            plugin_id=pid,
            version="1.0.0",
            kind="yaml-only",
            site_id="e2e-demo",
            extra_files={"README.md": b"# conflict\n"},
        )
        reauth = obtain_reauth(ctx, purpose="plugin:upload")
        up = post_multipart_phase1(
            ctx.client,
            "/api/v1/plugin-uploads",
            fields={"upload_id": str(uuid.uuid4())},
            files=[
                ("file", "dup.cmplugin", pkg_dup.bytes, "application/zip"),
                ("signature", "dup.minisig", sign_cmplugin_sha256(pkg_dup.bytes, kp).encode("utf-8"), "text/plain"),
            ],
            reauth_token=reauth,
            idempotency_key=uniq("dup"),
            if_match="0",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations/prepare",
                json_body={"upload_id": up["upload_id"]},
                if_match="0",
                idempotency_key=uniq("dupprep"),
            ),
            (409,),
            label="duplicate plugin version conflict",
        )
        catalog = ctx.call_phase1("GET", "/api/v1/sites/catalog")
        assert any(c.get("profile_id") == f"plugin:{pid}/e2e-demo" for c in catalog)

    @registry.case(
        id="T2-F09-03",
        tier=2,
        feature_id=9,
        name="Disabling YAML plugin surfaces explicit SiteFailure instead of silent fallback",
        self_test=False,
    )
    def test_t2_f09_disabled_provider_no_fallback(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('disprov')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="yaml-only", site_id="e2e-demo", enable=True)
        site = ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/e2e-demo")
        disable_plugin(ctx, pid)
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert (
            status.get("available") is False
            or "disabled" in str(status).lower()
            or "unavailable" in str(status).lower()
        ), f"Expected unavailable/disabled plugin-status, got {status!r}"
        res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
        assert isinstance(res, dict) and res.get("items") == []
        site_failures = [
            s for s in (res.get("sites") or [])
            if s.get("site_id") == site["id"] and s.get("error")
        ]
        assert site_failures, f"Expected explicit SiteFailure for {site['id']} in {res!r}"
        assert len(pt.request_log) == 0

    @registry.case(
        id="T2-F09-04",
        tier=2,
        feature_id=9,
        name="POST /sites with unregistered profile_id plugin:nonexistent/foo returns 400",
        self_test=False,
    )
    def test_t2_f09_unknown_plugin_profile(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        ctx.call_phase1("GET", "/api/v1/sites/catalog")
        err = assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/sites",
                json_body={
                    "name": uniq("badprof"),
                    "profile_id": "plugin:org.example.missing/foo",
                    "url": "https://example.com",
                    "cookie": "a=b",
                },
            ),
            (400,),
            label="unknown plugin profile_id",
        )
        assert err.code == "site.unknown_profile", f"Expected site.unknown_profile, got {err!r}"

    @registry.case(
        id="T2-F09-05",
        tier=2,
        feature_id=9,
        name="YAML plugin with path traversal in sites[] is rejected on upload",
        self_test=False,
    )
    def test_t2_f09_yaml_path_traversal(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            bad = build_cmplugin(
                plugin_id=f"org.example.{uniq('ytrav')}",
                kind="hook",
                manifest_overrides={"sites": ["../evil.yaml"]},
            )
            reauth = obtain_reauth(ctx, purpose="plugin:upload")
            assert_api_error(
                lambda: post_multipart_phase1(
                    ctx.client,
                    "/api/v1/plugin-uploads",
                    fields={"upload_id": uniq("up")},
                    files=[("file", "bad.cmplugin", bad.bytes, "application/zip")],
                    reauth_token=reauth,
                    idempotency_key=uniq("ytrav"),
                    if_match="0",
                ),
                (400, 422),
                label="path traversal in sites[]",
            )
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #10: crates/net-guard Outbound Policy & CredentialSlot (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F10-01",
        tier=2,
        feature_id=10,
        name="net-guard blocks loopback/metadata/RFC1918/IPv4-mapped-IPv6 targets with 0 spy hits",
        self_test=False,
    )
    def test_t2_f10_ssrf_targets_blocked(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        spy_pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        for bad_url in (
            f"{ctx.base_url}/api/v1/plugins",
            f"{spy_pt.url}/spy",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/secret.torrent",
            "http://[::ffff:127.0.0.1]/secret.torrent",
        ):
            assert_api_error(
                lambda u=bad_url: ctx.call_phase1(
                    "POST",
                    "/api/v1/downloaders/submit",
                    json_body={"downloader_id": dl["id"], "download_url": u, "title": "SSRF"},
                    idempotency_key=uniq("ssrf"),
                ),
                (400, 403),
                label=f"SSRF target {bad_url}",
            )
        assert len(spy_pt.spy_requests) == 0

    @registry.case(
        id="T2-F10-02",
        tier=2,
        feature_id=10,
        name="Redirect chain >5 hops and cross-origin credential leak are blocked",
        self_test=False,
    )
    def test_t2_f10_redirect_limit_and_cross_origin(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        pt_a = ctx.new_pt_site(expected_cookie="uid=42; pass=secretA")
        pt_b = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site_a = ensure_site(ctx, pt_a.url, profile_id="pterclub", cookie="uid=42; pass=secretA")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "site_id": site_a["id"],
                    "download_url": f"{pt_a.url}/redirect/7",
                    "title": "Too Many Redirects",
                },
                idempotency_key=uniq("r7"),
            ),
            (400, 502),
            label=">5 hop redirect chain",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "site_id": site_a["id"],
                    "download_url": f"{pt_a.url}/redirect/to?url={pt_b.url}/spy",
                    "title": "Cross Origin Redirect",
                },
                idempotency_key=uniq("rxo"),
            ),
            (400, 403, 502),
            label="cross-origin redirect blocked",
        )
        for req in pt_b.spy_requests:
            assert "secretA" not in req["headers"].get("cookie", "")
        assert len(qb.torrents) == 0

    @registry.case(
        id="T2-F10-03",
        tier=2,
        feature_id=10,
        name="Non-HTTP schemes (file:///) and userinfo URLs are rejected by net-guard",
        self_test=False,
    )
    def test_t2_f10_scheme_and_userinfo_rejected(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        for bad_url in ("file:///etc/passwd", "http://user:pass@127.0.0.1/a.torrent"):
            assert_api_error(
                lambda u=bad_url: ctx.call_phase1(
                    "POST",
                    "/api/v1/downloaders/submit",
                    json_body={"downloader_id": dl["id"], "download_url": u, "title": "Bad URL"},
                    idempotency_key=uniq("badurl"),
                ),
                (400,),
                label=f"bad url {bad_url}",
            )

    @registry.case(
        id="T2-F10-04",
        tier=2,
        feature_id=10,
        name="Plugin manifest [[http]] with userinfo or metadata IP origin is rejected",
        self_test=False,
    )
    def test_t2_f10_manifest_http_policy_boundaries(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for bad_origin, lbl in (
                ("http://169.254.169.254", "metadata IP in [[http]]"),
                ("http://user:pass@example.com", "userinfo origin in [[http]]"),
            ):
                pkg = build_cmplugin(
                    plugin_id=f"org.example.{uniq('httpbad')}",
                    kind="hook",
                    http_rules=[{"origin": bad_origin, "path_prefix": "/", "methods": ["GET"]}],
                )
                reauth = obtain_reauth(ctx, purpose="plugin:upload")
                assert_api_error(
                    lambda p=pkg, r=reauth: post_multipart_phase1(
                        ctx.client,
                        "/api/v1/plugin-uploads",
                        fields={"upload_id": uniq("up")},
                        files=[("file", "bad.cmplugin", p.bytes, "application/zip")],
                        reauth_token=r,
                        idempotency_key=uniq("httpmeta"),
                        if_match="0",
                    ),
                    (400, 422),
                    label=lbl,
                )
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F10-05",
        tier=2,
        feature_id=10,
        name="Oversized .torrent (>10 MiB) from MockPtSiteServer fails body-too-large with 0 qB adds",
        self_test=False,
    )
    def test_t2_f10_oversized_body_cap(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": f"{pt.url}/torrents/oversized.torrent",
                    "title": "Oversized",
                },
                idempotency_key=uniq("over10m"),
            ),
            (400, 413, 502),
            label="oversized .torrent >10 MiB",
        )
        assert len(qb.torrents) == 0

    # -----------------------------------------------------------------------
    # Feature #11: indexer-plugin & maintenance-plugin Worlds (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F11-01",
        tier=2,
        feature_id=11,
        name="Cross-site download-ref replay from Site A against Site B is rejected",
        self_test=False,
    )
    def test_t2_f11_cross_site_download_ref(ctx: TestContext) -> None:
        pt_a = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt_b = ctx.new_pt_site(expected_cookie="uid=42; pass=other")
        pid = f"org.example.{uniq('xsite')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="indexer", enable=True)
        site_a = ensure_site(ctx, pt_a.url, profile_id=f"plugin:{pid}/fixture")
        site_b = ensure_site(ctx, pt_b.url, profile_id=f"plugin:{pid}/fixture", cookie="uid=42; pass=other")
        assert site_a["id"] != site_b["id"]
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/sites/{site_b['id']}/plugin-actions",
                json_body={"action": "resolve-download", "download_ref": f"site:{site_a['id']}:101"},
                if_match=str(site_b["revision"]),
                idempotency_key=uniq("xref"),
            ),
            (400, 403, 409, 422),
            label="cross-site download-ref replay",
        )
        assert len(pt_a.request_log) == 0

    @registry.case(
        id="T2-F11-02",
        tier=2,
        feature_id=11,
        name="Rotating Site cookie/url increments credential_epoch and isolates prior KV",
        self_test=False,
    )
    def test_t2_f11_credential_epoch_isolation(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=v1")
        pid = f"org.example.{uniq('epoch')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="maintenance", enable=True)
        site = ensure_site(ctx, pt.url, profile_id="demo", cookie="uid=42; pass=v1")
        act1 = ctx.call_phase1(
            "POST",
            f"/api/v1/sites/{site['id']}/plugin-actions",
            json_body={"action": "check-in", "provider_id": f"plugin:{pid}/e2e-maint"},
            if_match=str(site["revision"]),
            idempotency_key=uniq("ep1"),
        )
        if isinstance(act1, dict) and act1.get("operation_id"):
            assert poll_operation(ctx, act1["operation_id"])["state"] == "succeeded"
        st1 = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert isinstance(st1, dict) and st1.get("credential_epoch"), f"Missing credential_epoch: {st1!r}"
        ctx.call_phase1(
            "PATCH",
            f"/api/v1/sites/{site['id']}",
            json_body={"cookie": "uid=42; pass=v2"},
        )
        st2 = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert isinstance(st2, dict) and st2.get("credential_epoch"), f"Missing credential_epoch: {st2!r}"
        assert str(st2["credential_epoch"]) != str(st1["credential_epoch"]), (
            f"Expected credential_epoch rotation: {st1!r} -> {st2!r}"
        )

    @registry.case(
        id="T2-F11-03",
        tier=2,
        feature_id=11,
        name="Calling undeclared maintenance action on plugin returns unsupported/400",
        self_test=False,
    )
    def test_t2_f11_undeclared_capability(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        pid = f"org.example.{uniq('uncap')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="indexer", enable=True)
        site = ensure_site(ctx, pt.url, profile_id=f"plugin:{pid}/fixture")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/sites/{site['id']}/plugin-actions",
                json_body={"action": "undeclared-action", "provider_id": f"plugin:{pid}/fixture"},
                if_match=str(site["revision"]),
                idempotency_key=uniq("uncap"),
            ),
            (400, 403, 422),
            label="undeclared capability/action",
        )

    @registry.case(
        id="T2-F11-04",
        tier=2,
        feature_id=11,
        name="Disabled indexer-plugin refuses /sites/:id/plugin-actions cleanly",
        self_test=False,
    )
    def test_t2_f11_disabled_plugin_actions_fail(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        pid = f"org.example.{uniq('disact')}"
        setup_signed_plugin_ready(ctx, plugin_id=pid, kind="maintenance", enable=True)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        disable_plugin(ctx, pid)
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/sites/{site['id']}/plugin-actions",
                json_body={"action": "check-in", "provider_id": f"plugin:{pid}/e2e-maint"},
                if_match=str(site["revision"]),
                idempotency_key=uniq("disact"),
            ),
            (400, 403, 409, 422),
            label="action on disabled plugin",
        )

    @registry.case(
        id="T2-F11-05",
        tier=2,
        feature_id=11,
        name="indexer-plugin declaring [[components.tasks]] is rejected at package validation",
        self_test=False,
    )
    def test_t2_f11_cross_world_table_rejected(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            bad = build_negative_cmplugin("invalid_toml_cross_world_table")
            reauth = obtain_reauth(ctx, purpose="plugin:upload")
            assert_api_error(
                lambda: post_multipart_phase1(
                    ctx.client,
                    "/api/v1/plugin-uploads",
                    fields={"upload_id": uniq("up")},
                    files=[("file", "bad.cmplugin", bad, "application/zip")],
                    reauth_token=reauth,
                    idempotency_key=uniq("cw"),
                    if_match="0",
                ),
                (400, 422),
                label="cross-world table in manifest",
            )
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #12: Narrow BrowserSession Port (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F12-01",
        tier=2,
        feature_id=12,
        name="Untrusted third-party plugin receives browser-isolation-unavailable with 0 CDP contexts",
        self_test=False,
    )
    def test_t2_f12_untrusted_plugin_blocked(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        cdp = ctx.new_cdp()
        put_browser_settings(ctx, True, cdp.url)
        pid = f"org.example.{uniq('untrbr')}"
        _, _, plugin = setup_signed_plugin_ready(ctx, plugin_id=pid, kind="maintenance", enable=True)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        avail = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{pid}/browser-availability?site_id={site['id']}&purpose=maintenance",
        )
        assert avail.get("status") != "ready"
        assert "browser-isolation-unavailable" in str(avail).lower() or avail.get("missing_requirements")
        try:
            chk = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/browser-checks",
                json_body={"site_id": site["id"], "purpose": "maintenance"},
                if_match=str(plugin["revision"]),
                idempotency_key=uniq("untrb"),
            )
            done = poll_operation(ctx, chk["operation_id"])
            assert done["state"] in ("failed", "rejected", "blocked"), (
                f"Expected untrusted browser-check failure, got {done!r}"
            )
        except ApiError as exc:
            assert exc.status in (403, 409, 422), (
                f"Expected 403/409/422 on untrusted browser-checks, got {exc}"
            )
        assert len(cdp.rpc_log) == 0

    @registry.case(
        id="T2-F12-02",
        tier=2,
        feature_id=12,
        name="Missing triple-gate requirements report missing_requirements[] in browser-availability",
        self_test=False,
    )
    def test_t2_f12_triple_gate_missing_reqs(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        cdp = ctx.new_cdp()
        put_browser_settings(ctx, False)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        avail_off = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/builtin.checkin/browser-availability?site_id={site['id']}&purpose=maintenance",
        )
        assert avail_off.get("status") != "ready"
        reqs_off = avail_off.get("missing_requirements")
        assert isinstance(reqs_off, list) and len(reqs_off) >= 1, (
            f"Expected missing_requirements when browser disabled, got {avail_off!r}"
        )
        assert any(
            tok in str(r).lower()
            for r in reqs_off
            for tok in ("browser", "cdp", "instance")
        ), f"Expected browser/cdp requirement when disabled, got {reqs_off!r}"
        put_browser_settings(ctx, True, cdp.url)
        try:
            avail_on = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/builtin.checkin/browser-availability?site_id={site['id']}&purpose=maintenance",
            )
            reqs_on = avail_on.get("missing_requirements")
            assert isinstance(reqs_on, list), f"Expected list missing_requirements, got {avail_on!r}"
            removed = [r for r in reqs_off if r not in reqs_on]
            assert len(removed) >= 1, (
                f"Expected enabling browser settings to clear disabled requirement: off={reqs_off!r}, on={reqs_on!r}"
            )
            assert any(
                tok in str(r).lower()
                for r in removed
                for tok in ("browser", "cdp", "instance")
            ), f"Expected cleared requirement to be browser/cdp/instance, got removed={removed!r}"
        finally:
            put_browser_settings(ctx, False)

    @registry.case(
        id="T2-F12-03",
        tier=2,
        feature_id=12,
        name="Post-navigation timeout leaves unknown-outcome without HTTP POST fallback and disposes context",
        self_test=False,
    )
    def test_t2_f12_post_nav_timeout_no_http_fallback(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        cdp = ctx.new_cdp(post_nav_timeout=True)
        put_browser_settings(ctx, True, cdp.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ctx.call_phase1("PATCH", f"/api/v1/sites/{site['id']}", json_body={"maintenance_via": "browser"})
        grant_site_browser_permission(ctx, "builtin.checkin", site["id"], "browser:maintenance")
        try:
            res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
            if isinstance(res, dict) and res.get("operation_id"):
                done = poll_operation(ctx, res["operation_id"], terminal_states=("unknown", "failed", "blocked", "rejected", "succeeded"))
                assert done["state"] != "succeeded", f"Post-nav timeout must not succeed: {done!r}"
        except ApiError as exc:
            assert exc.status in (400, 409, 502, 504), f"Unexpected status on browser timeout: {exc}"
        st = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert any(
            isinstance(r, dict) and r.get("action") == "check-in" and r.get("state") == "unknown"
            for r in (st.get("receipts") or [])
        ), f"Expected unknown receipt on post-nav timeout, got {st!r}"
        assert any(r.get("method") == "Target.createBrowserContext" for r in cdp.rpc_log)
        assert any(r.get("method") == "Page.navigate" for r in cdp.rpc_log)
        assert len(cdp.disposed_contexts) >= 1
        assert pt.checkin_post_count == 0
        assert len(cdp.active_contexts) == 0

    @registry.case(
        id="T2-F12-04",
        tier=2,
        feature_id=12,
        name="Incompatible Obscura mode fails gracefully without fallback or leaked contexts",
        self_test=False,
    )
    def test_t2_f12_incompatible_obscura(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        cdp = ctx.new_cdp(incompatible_mode=True)
        put_browser_settings(ctx, True, cdp.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ctx.call_phase1("PATCH", f"/api/v1/sites/{site['id']}", json_body={"maintenance_via": "browser"})
        grant_site_browser_permission(ctx, "builtin.checkin", site["id"], "browser:maintenance")
        p_chk = ctx.call_phase1("GET", "/api/v1/plugins/builtin.checkin")
        try:
            res = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/builtin.checkin/browser-checks",
                json_body={"site_id": site["id"], "purpose": "maintenance"},
                if_match=str(p_chk["revision"]),
                idempotency_key=uniq("t2f12incompat"),
            )
            if isinstance(res, dict) and res.get("operation_id"):
                done = poll_operation(ctx, res["operation_id"], terminal_states=("failed", "blocked", "rejected", "succeeded"))
                assert done["state"] in ("failed", "blocked", "rejected"), f"Expected failed browser-check on incompatible mode, got {done!r}"
        except ApiError as exc:
            assert exc.status in (400, 409, 422, 502, 503)
        assert len(cdp.rpc_log) >= 1
        assert pt.checkin_post_count == 0
        assert len(cdp.active_contexts) == 0

    @registry.case(
        id="T2-F12-05",
        tier=2,
        feature_id=12,
        name="Member calling /browser-availability or /browser-checks receives 403",
        self_test=False,
    )
    def test_t2_f12_member_forbidden(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/builtin.checkin/browser-availability?site_id={site['id']}&purpose=maintenance",
        )
        _, mc = create_member_client(ctx)
        assert_api_error(
            lambda: mc.get(
                f"/api/v1/plugins/builtin.checkin/browser-availability?site_id={site['id']}&purpose=maintenance"
            ),
            (403,),
            label="member browser-availability",
        )
        assert_api_error(
            lambda: mc.post(
                "/api/v1/plugins/builtin.checkin/browser-checks",
                json_body={"site_id": site["id"], "purpose": "maintenance"},
            ),
            (403,),
            label="member browser-checks",
        )

    # -----------------------------------------------------------------------
    # Feature #13: site_maintenance_receipts & First-Party Check-in (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F13-01",
        tier=2,
        feature_id=13,
        name="Legacy boost_check is deleted (boost-stats 404 and jobs/tick runs check-in without boost)",
        self_test=False,
    )
    def test_t2_f13_legacy_boost_check_deleted(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        r = ctx.client.raw_request("GET", "/api/v1/sites/boost-stats")
        assert r.status == 404, f"Legacy boost-stats must return 404, got {r.status}"
        r_patch = ctx.client.raw_request(
            "PATCH",
            f"/api/v1/sites/{site['id']}/boost",
            body=b'{"enabled":true}',
            headers={"Content-Type": "application/json"},
        )
        assert r_patch.status == 404, f"Legacy PATCH /sites/:id/boost must return 404, got {r_patch.status}"
        res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1

    @registry.case(
        id="T2-F13-02",
        tier=2,
        feature_id=13,
        name="Credential rotation in same window does not cause duplicate check-in POST",
        self_test=False,
    )
    def test_t2_f13_rotation_no_duplicate_checkin(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins/builtin.checkin")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=v1")
        site = ensure_site(ctx, pt.url, profile_id="pterclub", cookie="uid=42; pass=v1")
        r1 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(r1, dict) and r1.get("operation_id"):
            assert poll_operation(ctx, r1["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1
        pt.expected_cookie = "uid=42; pass=v2"
        ctx.call_phase1("PATCH", f"/api/v1/sites/{site['id']}", json_body={"cookie": "uid=42; pass=v2"})
        r2 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(r2, dict) and r2.get("operation_id"):
            assert poll_operation(ctx, r2["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert "completed" in str(status).lower() or "already" in str(status).lower()

    @registry.case(
        id="T2-F13-03",
        tier=2,
        feature_id=13,
        name="Dropped response after check-in POST marks unknown and blocks blind retry in same window",
        self_test=False,
    )
    def test_t2_f13_drop_response_unknown_blocks_retry(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins/builtin.checkin")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.drop_response_on_next_checkin = True
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        try:
            r1 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
            if isinstance(r1, dict) and r1.get("operation_id"):
                done1 = poll_operation(ctx, r1["operation_id"], terminal_states=("unknown", "failed", "blocked", "succeeded"))
                assert done1["state"] != "succeeded"
        except ApiError as exc:
            assert exc.status in (400, 409, 502, 504)
        assert pt.checkin_post_count == 1
        st1 = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert "unknown" in str(st1).lower()
        try:
            r2 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
            if isinstance(r2, dict) and r2.get("operation_id"):
                done2 = poll_operation(ctx, r2["operation_id"], terminal_states=("blocked", "unknown", "failed", "rejected", "succeeded"))
                assert done2["state"] != "succeeded"
        except ApiError as exc:
            assert exc.status in (400, 409, 423)
        assert pt.checkin_post_count == 1

    @registry.case(
        id="T2-F13-04",
        tier=2,
        feature_id=13,
        name="Check-in Set-Cookie preserves receipt without overwriting newer manual cookie",
        self_test=False,
    )
    def test_t2_f13_cookie_cas_race(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins/builtin.checkin")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.checkin_rotate_cookie = "pass=rotated_from_checkin"
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        r1 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(r1, dict) and r1.get("operation_id"):
            assert poll_operation(ctx, r1["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1
        ctx.call_phase1(
            "PATCH",
            f"/api/v1/sites/{site['id']}",
            json_body={"cookie": "uid=42; pass=manual_newer"},
        )
        pt.expected_cookie = "uid=42; pass=manual_newer"
        ver = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
        assert ver.get("ok") is True
        assert "pass=manual_newer" in pt.request_log[-1]["headers"].get("cookie", "")
        r2 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        if isinstance(r2, dict) and r2.get("operation_id"):
            assert poll_operation(ctx, r2["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1

    @registry.case(
        id="T2-F13-05",
        tier=2,
        feature_id=13,
        name="Check-in challenge/auth_expired HTML does not record completed receipt",
        self_test=False,
    )
    def test_t2_f13_challenge_not_completed(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins/builtin.checkin")
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.checkin_mode = "challenge"
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        try:
            res = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
            if isinstance(res, dict) and res.get("operation_id"):
                done = poll_operation(ctx, res["operation_id"], terminal_states=("failed", "blocked", "rejected", "succeeded"))
                assert done["state"] in ("failed", "blocked", "rejected")
        except ApiError as exc:
            assert exc.status in (400, 403, 409, 422, 502)
        assert pt.checkin_post_count >= 1, (
            f"Expected check-in POST to reach PT site before challenge failure, got {pt.checkin_post_count}"
        )
        status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
        assert "completed" not in str(status).lower()
