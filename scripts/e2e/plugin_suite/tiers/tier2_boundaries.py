"""Tier 2: Boundary Value Analysis & Negative/Error Tests (Features #1-#26)."""
from __future__ import annotations

import uuid

from ..lib.client import ApiError
from ..lib.fixtures import (
    NEGATIVE_CMPLUGIN_KINDS,
    build_negative_cmplugin,
    build_overflow_torrent,
    build_torrent_bytes,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
    verify_minisign,
)
from . import (
    SuiteRegistry,
    TestContext,
    UnimplementedFeatureError,
    t2_p1c,
    t2_p2,
    t2_p3_p4,
    t2_p5,
    t2_p6,
)
from ._helpers import InstanceSupervisor, make_temp_web_dir, resolve_bin_path


def register_cases(registry: SuiteRegistry) -> None:
    @registry.case(
        id="T2-F24-SMOKE-01",
        tier=2,
        feature_id=24,
        name="Cookie-only asset auth, member 403, nosniff/no-store headers, and /plugins vs /plugin-apps route separation",
        self_test=True,
    )
    def test_t2_ui_asset_and_namespace_boundaries(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-ui-contract")

        asset_path = "/api/v1/plugin-ui-assets/org.example.stub-policy/deadbeef/ui/assets/remoteEntry.js"

        anon_resp = ctx.client.raw_request("GET", asset_path, no_bearer=True, no_cookie=True)
        if anon_resp.status in (404, 405, 501):
            raise UnimplementedFeatureError(f"GET {asset_path}", f"HTTP {anon_resp.status}")
        assert anon_resp.status == 401, f"Expected 401 without cookie, got {anon_resp.status}"
        assert "html" not in anon_resp.header("content-type", "").lower()

        member_login = f"e2e_m_{uuid.uuid4().hex[:8]}"
        member_pw = "member-secret-pw-123"
        ctx.client.create_member(member_login, member_pw)
        member_client = ctx.new_client()
        member_client.login(member_login, member_pw)
        member_asset = member_client.get_asset(asset_path, use_cookie=True, no_bearer=True)
        if member_asset.status in (404, 405, 501):
            raise UnimplementedFeatureError(f"GET {asset_path}", f"HTTP {member_asset.status}")
        assert member_asset.status == 403, f"Expected 403 for member asset read, got {member_asset.status}"

        cookie_client = ctx.client.clone_cookie_only()
        assert cookie_client.session_cookie, "Expected mc_session cookie on admin client"
        ok_resp = cookie_client.get_asset(asset_path, use_cookie=True, no_bearer=True)
        assert ok_resp.status in (200, 404), f"Expected 200 or 404 for unknown stub asset, got {ok_resp.status}"
        if ok_resp.status == 200:
            assert ok_resp.header("x-content-type-options") == "nosniff"
            assert "no-store" in (ok_resp.header("cache-control") or "")

        jf_resp = ctx.client.raw_request("GET", "/plugins/org.example.stub/main", no_bearer=True, no_cookie=True)
        assert jf_resp.status == 404, f"Expected 404 for Jellyfin /plugins/* path, got {jf_resp.status}"
        assert "html" not in jf_resp.header("content-type", "").lower()

        if ctx.supervisor is not None and ctx.web_dir is None:
            web_dir = make_temp_web_dir(ctx)
            with InstanceSupervisor(
                bin_path=resolve_bin_path(ctx),
                web_dir=web_dir,
                cli_token=ctx.cli_token,
                admin_password=ctx.admin_password,
                keep_tmp=ctx.keep_tmp,
            ) as ui_sup:
                app_resp = ui_sup.client().raw_request(
                    "GET", "/plugin-apps/org.example.stub/main", no_bearer=True, no_cookie=True
                )
                assert app_resp.status == 200, f"Expected 200 SPA HTML for /plugin-apps/*, got {app_resp.status}"
                assert "text/html" in (app_resp.header("content-type") or "").lower()
        else:
            app_resp = ctx.client.raw_request(
                "GET", "/plugin-apps/org.example.stub/main", no_bearer=True, no_cookie=True
            )
            assert app_resp.status == 200, f"Expected 200 SPA HTML for /plugin-apps/*, got {app_resp.status}"
            assert "text/html" in (app_resp.header("content-type") or "").lower()

    @registry.case(
        id="T2-F01-SMOKE-02",
        tier=2,
        feature_id=1,
        name="Negative .cmplugin structural hazards, tampered minisign, and .torrent i64 overflow boundary",
        self_test=True,
    )
    def test_t2_fixture_boundaries(ctx: TestContext) -> None:
        plugins = ctx.call_phase1("GET", "/api/v1/plugins")
        assert isinstance(plugins, list), f"Expected list from /api/v1/plugins, got {type(plugins)}"

        for kind in ("path_traversal", "absolute_path", "symlink", "duplicate_paths", "case_collision", "encrypted_entry", "empty_package"):
            assert kind in NEGATIVE_CMPLUGIN_KINDS
            bad_zip = build_negative_cmplugin(kind)
            assert len(bad_zip) > 22, f"Negative fixture {kind} produced invalid short bytes"

        kp = generate_minisign_keypair(seed=b"\x22" * 32)
        t = build_torrent_bytes(name="boundary-file", length=4096)
        tampered_sig = sign_cmplugin_sha256(t.bytes, kp, hashed=True, tamper_sig=True)
        ver = verify_minisign(bytes.fromhex(t.bytes_digest), tampered_sig, kp.public_key_text)
        assert not ver.valid

        ov = build_overflow_torrent()
        assert ov.verified_total_bytes == 2 ** 63

        anon = ctx.new_client()
        try:
            anon.me()
            raise AssertionError("Expected 401 from unauthenticated /auth/me")
        except ApiError as exc:
            assert exc.status == 401

    t2_p1c.register_cases(registry)
    t2_p2.register_cases(registry)
    t2_p3_p4.register_cases(registry)
    t2_p5.register_cases(registry)
    t2_p6.register_cases(registry)
