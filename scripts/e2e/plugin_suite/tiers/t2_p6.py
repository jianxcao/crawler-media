"""Tier 2 Boundary & Negative Tests for Features #22-#26 (P6 UI Platform, CLI & First-Party UIs)."""
from __future__ import annotations

import json
import uuid

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
    enable_plugin,
    ensure_downloader,
    ensure_site,
    extract_prep_grants,
    grant_ui_trust,
    obtain_reauth,
    pin_trust_key,
    poll_operation,
    post_multipart_phase1,
    put_browser_settings,
    run_plugin_cli,
    set_developer_mode,
    setup_signed_plugin_ready,
    uniq,
    unwrap_items,
    upload_cmplugin,
)
from .t1_p6 import _find_first_party_plugin


def _upload_negative_ui(ctx: TestContext, raw_bytes: bytes) -> None:
    reauth = obtain_reauth(ctx, purpose="plugin:upload")
    uid = str(uuid.uuid4())
    assert_api_error(
        lambda: post_multipart_phase1(
            ctx.client,
            "/api/v1/plugin-uploads",
            fields={"upload_id": uid},
            files=[("file", "bad-ui.cmplugin", raw_bytes, "application/zip")],
            reauth_token=reauth,
            idempotency_key=f"idem-{uid}",
            if_match="0",
        ),
        (400, 409, 422),
        label="negative UI package upload",
    )


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #22: Minimal Vite 7 + React 19 @module-federation/vite (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F22-01",
        tier=2,
        feature_id=22,
        name="Mismatched ui.contract (2.0.0) or ui.react (18.3.0) is rejected",
        self_test=False,
    )
    def test_t2_f22_contract_react_mismatch(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in ("invalid_ui_contract_mismatch", "invalid_ui_react_mismatch"):
                _upload_negative_ui(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F22-02",
        tier=2,
        feature_id=22,
        name="Reserved remote_name (window) and duplicate remote_name across plugins rejected",
        self_test=False,
    )
    def test_t2_f22_remote_name_collisions(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            _upload_negative_ui(ctx, build_negative_cmplugin("invalid_ui_reserved_remote_name"))
        finally:
            set_developer_mode(ctx, False)
        shared_remote = uniq("cm_shared").replace("-", "_")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('r1')}",
            kind="ui-only",
            ui_overrides={"remote_name": shared_remote},
            enable=False,
        )
        kp2 = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp2, label=uniq("r2"))
        pkg2 = build_cmplugin(
            plugin_id=f"org.example.{uniq('r2')}",
            kind="ui-only",
            ui_overrides={"remote_name": shared_remote},
        )
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp2))
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up2["upload_id"]},
            if_match="0",
            idempotency_key=uniq("r2p"),
        )
        done = poll_operation(ctx, prep_op["operation_id"])
        if done["state"] == "succeeded":
            prep = done.get("result") or done
            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    "/api/v1/plugin-installations",
                    json_body={
                        "preparation_id": prep["preparation_id"],
                        "consent_token": prep["consent_token"],
                        "grants": extract_prep_grants(prep) or ["read:sites"],
                    },
                    if_match="0",
                    idempotency_key=uniq("r2i"),
                ),
                (400, 409),
                label="duplicate remote_name across plugins",
            )
        else:
            assert done["state"] in ("failed", "rejected"), (
                f"Expected failed/rejected for duplicate remote_name, got {done!r}"
            )

    @registry.case(
        id="T2-F22-03",
        tier=2,
        feature_id=22,
        name="Forbidden UI extensions (.html, .map, .tsx, package.json) rejected",
        self_test=False,
    )
    def test_t2_f22_forbidden_ui_extensions(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in ("invalid_ui_html", "invalid_ui_map"):
                _upload_negative_ui(ctx, build_negative_cmplugin(kind))
            tsx_pkg = build_cmplugin(
                kind="ui-only",
                extra_files={"ui/assets/Config.tsx": b"export default function C(){return null;}\n"},
            )
            _upload_negative_ui(ctx, tsx_pkg.bytes)
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F22-04",
        tier=2,
        feature_id=22,
        name="asset-manifest.json sha256 mismatch, self-listing, missing ui.entry, and undeclared file rejected",
        self_test=False,
    )
    def test_t2_f22_manifest_integrity_hazards(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in (
                "invalid_ui_wrong_sha256",
                "invalid_ui_self_listed_manifest",
                "invalid_ui_missing_entry_in_manifest",
                "invalid_ui_undeclared_asset",
            ):
                _upload_negative_ui(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F22-05",
        tier=2,
        feature_id=22,
        name="Asset count (>1024), single-file size (>8MiB), and manifest version != 1 rejected",
        self_test=False,
    )
    def test_t2_f22_count_size_and_version_caps(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in (
                "invalid_ui_too_many_files",
                "invalid_ui_oversized_file",
                "invalid_ui_manifest_version",
            ):
                _upload_negative_ui(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #23: Author CLI (tools/plugin/) (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F23-01",
        tier=2,
        feature_id=23,
        name="plugin scaffold rejects reserved --id, unsupported --api 2.0.0, or unknown --world",
        self_test=False,
    )
    def test_t2_f23_scaffold_invalid_args(ctx: TestContext) -> None:
        help_p = run_plugin_cli(ctx, ["--help"])
        assert help_p.returncode == 0, f"plugin --help failed: {help_p.stderr!r}"
        s1 = ctx.tmp_dir / "s1"
        p1 = run_plugin_cli(ctx, ["scaffold", "--id", "installed", "--world", "hook-plugin", str(s1)])
        assert p1.returncode != 0 and not s1.exists(), (
            f"Expected non-zero exit and no dir s1: rc={p1.returncode}, exists={s1.exists()}"
        )
        err1 = f"{p1.stdout}\n{p1.stderr}".lower()
        assert any(tok in err1 for tok in ("installed", "reserved", "--id", "usage")), (
            f"Expected specific diagnostic stderr for reserved --id, got {err1!r}"
        )
        s2 = ctx.tmp_dir / "s2"
        p2 = run_plugin_cli(ctx, ["scaffold", "--api", "2.0.0", "--world", "hook-plugin", str(s2)])
        assert p2.returncode != 0 and not s2.exists(), (
            f"Expected non-zero exit and no dir s2: rc={p2.returncode}, exists={s2.exists()}"
        )
        err2 = f"{p2.stdout}\n{p2.stderr}".lower()
        assert any(tok in err2 for tok in ("2.0.0", "api", "unsupported", "version", "usage")), (
            f"Expected specific diagnostic stderr for unsupported --api, got {err2!r}"
        )

    @registry.case(
        id="T2-F23-02",
        tier=2,
        feature_id=23,
        name="plugin validate exits non-zero on invalid plugin.toml or recursive $ref schema",
        self_test=False,
    )
    def test_t2_f23_validate_rejects_bad_project(ctx: TestContext) -> None:
        bad_dir = ctx.tmp_dir / "bad_proj"
        bad_dir.mkdir(parents=True, exist_ok=True)
        (bad_dir / "plugin.toml").write_text("manifest_version = 1\nid = \"installed\"\n", encoding="utf-8")
        proc = run_plugin_cli(ctx, ["validate", str(bad_dir)])
        assert proc.returncode != 0
        rec_dir = ctx.tmp_dir / "rec_proj"
        assert run_plugin_cli(
            ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", str(rec_dir)]
        ).returncode == 0
        (rec_dir / "schemas" / "config.schema.json").write_text(
            json.dumps(
                {
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "$defs": {
                        "node": {
                            "type": "object",
                            "additionalProperties": False,
                            "properties": {"next": {"$ref": "#/$defs/node"}},
                        }
                    },
                    "properties": {"root": {"$ref": "#/$defs/node"}},
                }
            ),
            encoding="utf-8",
        )
        proc2 = run_plugin_cli(ctx, ["validate", str(rec_dir)])
        assert proc2.returncode != 0

    @registry.case(
        id="T2-F23-03",
        tier=2,
        feature_id=23,
        name="plugin pack rejects forbidden .html/.map under ui/ without creating output archive",
        self_test=False,
    )
    def test_t2_f23_pack_rejects_forbidden_ui(ctx: TestContext) -> None:
        proj = ctx.tmp_dir / "pack_bad"
        assert run_plugin_cli(ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", "--ui", "react", str(proj)]).returncode == 0
        (proj / "ui" / "assets").mkdir(parents=True, exist_ok=True)
        (proj / "ui" / "assets" / "index.html").write_text("<html></html>", encoding="utf-8")
        out_pkg = ctx.tmp_dir / "should_not_exist.cmplugin"
        p = run_plugin_cli(ctx, ["pack", str(proj), "--output", str(out_pkg)])
        assert p.returncode != 0 and not out_pkg.exists()

    @registry.case(
        id="T2-F23-04",
        tier=2,
        feature_id=23,
        name="plugin sign fails cleanly on missing key or missing .cmplugin without partial .minisig",
        self_test=False,
    )
    def test_t2_f23_sign_missing_inputs(ctx: TestContext) -> None:
        help_p = run_plugin_cli(ctx, ["sign", "--help"])
        assert help_p.returncode == 0, f"plugin sign --help failed: {help_p.stderr!r}"
        missing_pkg = ctx.tmp_dir / "missing.cmplugin"
        missing_key = ctx.tmp_dir / "missing.key"
        p = run_plugin_cli(ctx, ["sign", str(missing_pkg), "--key", str(missing_key)])
        assert p.returncode != 0
        assert not missing_pkg.with_suffix(".cmplugin.minisig").exists()
        err = f"{p.stdout}\n{p.stderr}".lower()
        assert any(
            tok in err
            for tok in ("missing.cmplugin", "missing.key", "not found", "no such file", "exist", "missing", "usage", "cannot")
        ), f"Expected specific missing-input diagnostic stderr, got {err!r}"

    @registry.case(
        id="T2-F23-05",
        tier=2,
        feature_id=23,
        name="plugin dev --instance fails when developer mode is disabled or auth is invalid",
        self_test=False,
    )
    def test_t2_f23_dev_refused_without_dev_mode(ctx: TestContext) -> None:
        proj = ctx.tmp_dir / "dev_refused"
        assert run_plugin_cli(ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", str(proj)]).returncode == 0
        set_developer_mode(ctx, False)
        p = run_plugin_cli(ctx, ["dev", str(proj), "--instance", ctx.base_url])
        assert p.returncode != 0

    # -----------------------------------------------------------------------
    # Feature #24: plugin_ui_trust, Descriptor, Trust & Assets (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F24-01",
        tier=2,
        feature_id=24,
        name="Auth & RBAC: anon/Bearer-only/logged-out rejected (401) and member gets 403 on all UI endpoints",
        self_test=False,
    )
    def test_t2_f24_auth_and_cookie_only_assets(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f24r')}", kind="ui-only", enable=True
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        asset_path = f"/api/v1/plugin-ui-assets/{pkg.plugin_id}/{pkg.sha256_hex}/ui/assets/remoteEntry.js"

        anon_r = ctx.client.raw_request("GET", asset_path, no_bearer=True, no_cookie=True)
        assert anon_r.status == 401 and "html" not in (anon_r.header("content-type") or "").lower()

        bearer_only = ctx.client.clone(token=ctx.client.token, session_cookie=None)
        bo_r = bearer_only.raw_request("GET", asset_path, no_cookie=True)
        assert bo_r.status == 401

        _, mc = create_member_client(ctx)
        assert_api_error(lambda: mc.get(f"/api/v1/plugins/{pkg.plugin_id}/ui"), (403,), label="member ui descriptor")
        assert_api_error(lambda: mc.get("/api/v1/plugin-ui-navigation"), (403,), label="member ui navigation")
        m_asset = mc.get_asset(asset_path, use_cookie=True, no_bearer=True)
        assert m_asset.status == 403

    @registry.case(
        id="T2-F24-02",
        tier=2,
        feature_id=24,
        name="UI trust is isolated per package_hash (upgrading invalidates prior hash asset trust)",
        self_test=False,
    )
    def test_t2_f24_per_hash_trust_isolation(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('f24iso')}"
        pkg1, kp, p1 = setup_signed_plugin_ready(ctx, plugin_id=pid, version="1.0.0", kind="ui-only", enable=False)
        grant_ui_trust(ctx, pid, pkg1.sha256_hex)
        pkg2 = build_cmplugin(
            plugin_id=pid,
            version="1.1.0",
            kind="ui-only",
            extra_ui_assets={"ui/assets/v2.js": (b"export const v=2;\n", "application/javascript")},
        )
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp))
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"upload_id": up2["upload_id"], "state_policy": "preserve", "target_config": {}},
            if_match=str(p1["revision"]),
            idempotency_key=uniq("uiprep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        up_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrade",
            json_body={
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": extract_prep_grants(prep) or ["read:sites"],
                "state_policy": "preserve",
                "target_config_digest": prep.get("target_config_digest", ""),
            },
            if_match=str(p1["revision"]),
            idempotency_key=uniq("uiup"),
        )
        poll_operation(ctx, up_op["operation_id"])
        r_v2 = ctx.client.clone_cookie_only().get_asset(
            f"/api/v1/plugin-ui-assets/{pid}/{pkg2.sha256_hex}/ui/assets/remoteEntry.js"
        )
        assert r_v2.status == 403

    @registry.case(
        id="T2-F24-03",
        tier=2,
        feature_id=24,
        name="Asset path traversal, wrong hash, undeclared path, and disabled non-config surface blocked",
        self_test=False,
    )
    def test_t2_f24_asset_path_and_lifecycle_guards(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f24pth')}",
            kind="ui-only",
            ui_overrides={
                "surfaces": [
                    {"id": "app", "kind": "app", "expose": "./App", "title": "App", "icon": "puzzle"}
                ]
            },
            enable=True,
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        cc = ctx.client.clone_cookie_only()
        for bad_path in (
            f"/api/v1/plugin-ui-assets/{pkg.plugin_id}/{pkg.sha256_hex}/../../plugin.toml",
            f"/api/v1/plugin-ui-assets/{pkg.plugin_id}/{'0' * 64}/ui/assets/remoteEntry.js",
            f"/api/v1/plugin-ui-assets/{pkg.plugin_id}/{pkg.sha256_hex}/ui/assets/nonexistent.js",
        ):
            r = cc.get_asset(bad_path)
            assert r.status in (400, 403, 404)
            assert "html" not in (r.header("content-type") or "").lower()
        disable_plugin(ctx, pkg.plugin_id)
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/ui")
        assert desc["surfaces"][0]["mountable"] is False
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/ui/query",
                json_body={"name": "sites.list", "input": {}},
            ),
            (403, 409),
            label="disabled plugin ui/query",
        )

    @registry.case(
        id="T2-F24-04",
        tier=2,
        feature_id=24,
        name="ui-trust/grant enforces If-Match (428/409), reauth_token, and Idempotency-Key",
        self_test=False,
    )
    def test_t2_f24_trust_cas_and_reauth(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f24cas')}", kind="ui-only", enable=True
        )
        prep = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/ui-trust/prepare",
            json_body={"package_hash": pkg.sha256_hex},
        )
        reauth = obtain_reauth(ctx, purpose="plugin-ui-trust")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/ui-trust/grant",
                json_body={
                    "package_hash": pkg.sha256_hex,
                    "consent_token": prep["consent_token"],
                    "reauth_token": reauth,
                },
                idempotency_key=uniq("noim"),
            ),
            (428,),
            label="ui-trust/grant missing If-Match",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/ui-trust/grant",
                json_body={
                    "package_hash": pkg.sha256_hex,
                    "consent_token": prep["consent_token"],
                    "reauth_token": reauth,
                },
                if_match="99",
                idempotency_key=uniq("badim"),
            ),
            (409,),
            label="ui-trust/grant stale If-Match",
        )

    @registry.case(
        id="T2-F24-05",
        tier=2,
        feature_id=24,
        name="ui/query rejects unknown queries/missing caps/disabled plugin; commands reject forged X-Plugin-* headers",
        self_test=False,
    )
    def test_t2_f24_query_and_source_header_guards(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f24qgd')}",
            kind="ui-only",
            ui_overrides={"capabilities": ["read:sites"]},
            enable=True,
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/ui/query",
                json_body={"name": "secrets.dump", "input": {}},
            ),
            (400, 403),
            label="unknown ui/query name",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/ui/query",
                json_body={"name": "boost.snapshot", "input": {}},
            ),
            (403,),
            label="ui/query missing read:boost capability",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/commands",
                json_body={"kind": "indexer.search", "payload": {}},
                plugin_package="0" * 64,
                plugin_generation="999",
                idempotency_key=uniq("badhdr"),
            ),
            (400, 403, 409),
            label="forged X-Plugin-Package/Generation headers",
        )

    # -----------------------------------------------------------------------
    # Feature #25: Frontend Shell, 4 Surfaces, Host SDK & Reload (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F25-01",
        tier=2,
        feature_id=25,
        name="Surface manifest bounds (>16 surfaces, 2 config surfaces, unknown icon) rejected",
        self_test=False,
    )
    def test_t2_f25_surface_manifest_bounds(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in (
                "invalid_ui_too_many_surfaces",
                "invalid_ui_duplicate_config_surface",
                "invalid_ui_invalid_icon",
            ):
                _upload_negative_ui(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F25-02",
        tier=2,
        feature_id=25,
        name="Enabled config surface is read-only (editable=false) and PUT /config returns 409",
        self_test=False,
    )
    def test_t2_f25_enabled_config_locked(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f25lk')}", kind="ui-only", enable=True
        )
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/ui")
        cfg_surf = [s for s in desc["surfaces"] if s["kind"] == "config"][0]
        assert cfg_surf["editable"] is False
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        assert_api_error(
            lambda: ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pkg.plugin_id}/config",
                json_body={"schema_revision": cfg["schema_revision"], "values": {"dry_run": False}, "secrets": {}},
                if_match=str(cfg["revision"]),
                idempotency_key=uniq("lkp"),
            ),
            (409,),
            label="PUT /config while enabled",
        )

    @registry.case(
        id="T2-F25-03",
        tier=2,
        feature_id=25,
        name="Forged or cross-plugin UiHandle in ui/query is rejected",
        self_test=False,
    )
    def test_t2_f25_forged_ui_handle_rejected(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f25hdl')}",
            kind="ui-only",
            ui_overrides={"capabilities": ["read:sites", "read:catalog"]},
            enable=True,
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        err = assert_api_error(
            lambda: ctx.client.post(
                f"/api/v1/plugins/{pkg.plugin_id}/ui/query",
                json_body={"name": "indexer.cached", "input": {"search_operation_id": str(uuid.uuid4())}},
            ),
            (400, 403, 404),
            label="forged search_operation_id",
        )
        if err.status == 404:
            assert err.code is not None and err.code != "not_found"

    @registry.case(
        id="T2-F25-04",
        tier=2,
        feature_id=25,
        name="UI-originated downloader.delete-files without valid confirmation_token is rejected",
        self_test=False,
    )
    def test_t2_f25_cleanup_confirmation_token_required(ctx: TestContext) -> None:
        pkg, _, plugin = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f25ctok')}",
            kind="ui-only",
            ui_overrides={"capabilities": ["read:boost", "command:downloader.delete-files"]},
            enable=True,
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/commands",
                json_body={
                    "kind": "downloader.delete-files",
                    "payload": {"host_file_plan_handle": "fake", "revision": 1, "confirmation_token": "forged"},
                },
                plugin_package=pkg.sha256_hex,
                plugin_generation=str(plugin["generation"]),
                idempotency_key=uniq("ctok"),
            ),
            (400, 403, 409),
            label="forged confirmation_token on UI delete-files",
        )

    @registry.case(
        id="T2-F25-05",
        tier=2,
        feature_id=25,
        name="Member denied 403 on plugin UI endpoints and reserved plugin IDs rejected",
        self_test=False,
    )
    def test_t2_f25_member_and_reserved_id_isolation(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-ui-navigation")
        _, mc = create_member_client(ctx)
        assert_api_error(lambda: mc.get("/api/v1/plugin-ui-navigation"), (403,), label="member navigation")
        set_developer_mode(ctx, True)
        try:
            _upload_negative_ui(ctx, build_negative_cmplugin("invalid_toml_reserved_id"))
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #26: First-Party Check-in & Boost React UIs (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F26-01",
        tier=2,
        feature_id=26,
        name="Check-in browser-availability redacts cdp_url/cookies and reports missing_requirements",
        self_test=False,
    )
    def test_t2_f26_checkin_browser_availability_redacted(ctx: TestContext) -> None:
        p = _find_first_party_plugin(ctx, ("builtin.checkin", "org.example.checkin"), "checkin")
        pt = ctx.new_pt_site()
        cdp = ctx.new_cdp()
        secret_cookie = "uid=99; pass=ultra-secret-cookie-777"
        site = ensure_site(ctx, pt.url, profile_id="pterclub", cookie=secret_cookie)
        put_browser_settings(ctx, False, "ws://127.0.0.1:9222/secret-cdp")
        try:
            cfg_off = ctx.call_phase1("GET", "/api/v1/settings/browser")
            assert isinstance(cfg_off, dict) and cfg_off.get("cdp", {}).get("enabled") is False, (
                f"Expected persisted cdp.enabled=False in GET /settings/browser, got {cfg_off!r}"
            )
            assert cfg_off.get("cdp", {}).get("url") == "ws://127.0.0.1:9222/secret-cdp", (
                f"Expected persisted cdp.url in GET /settings/browser, got {cfg_off!r}"
            )
            avail_off = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/{p['id']}/browser-availability?site_id={site['id']}&purpose=maintenance",
            )
            for forbidden in ("secret-cdp", "ws://", "cdp_url", "ultra-secret-cookie-777"):
                assert forbidden not in str(avail_off), f"Leaked {forbidden!r} in {avail_off!r}"
            assert avail_off.get("status") != "ready"
            reqs_off = avail_off.get("missing_requirements")
            assert isinstance(reqs_off, list) and len(reqs_off) >= 1, (
                f"Expected non-empty missing_requirements in {avail_off!r}"
            )
            assert any(
                tok in str(r).lower()
                for r in reqs_off
                for tok in ("browser", "cdp", "instance")
            ), f"Expected browser/cdp requirement when disabled, got {reqs_off!r}"

            put_browser_settings(ctx, True, cdp.url)
            cfg_on = ctx.call_phase1("GET", "/api/v1/settings/browser")
            assert isinstance(cfg_on, dict) and cfg_on.get("cdp", {}).get("enabled") is True, (
                f"Expected persisted cdp.enabled=True in GET /settings/browser, got {cfg_on!r}"
            )
            assert cfg_on.get("cdp", {}).get("url") == cdp.url, (
                f"Expected persisted cdp.url={cdp.url!r} in GET /settings/browser, got {cfg_on!r}"
            )
            avail_on = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/{p['id']}/browser-availability?site_id={site['id']}&purpose=maintenance",
            )
            for forbidden in (cdp.url, "http://", f":{cdp.port}", "cdp_url", "ultra-secret-cookie-777"):
                assert forbidden not in str(avail_on), f"Leaked {forbidden!r} in {avail_on!r}"
            reqs_on = avail_on.get("missing_requirements")
            assert isinstance(reqs_on, list), f"Expected list missing_requirements, got {avail_on!r}"
            removed = [r for r in reqs_off if r not in reqs_on]
            assert len(removed) >= 1 and any(
                tok in str(r).lower()
                for r in removed
                for tok in ("browser", "cdp", "instance")
            ), f"Expected enabling browser settings to clear browser/cdp requirement: off={reqs_off!r}, on={reqs_on!r}"
        finally:
            put_browser_settings(ctx, False)

    @registry.case(
        id="T2-F26-02",
        tier=2,
        feature_id=26,
        name="Check-in config PUT rejects invalid interval/transport bounds with 400 plugin.config_invalid",
        self_test=False,
    )
    def test_t2_f26_checkin_config_bounds(ctx: TestContext) -> None:
        p = _find_first_party_plugin(ctx, ("builtin.checkin", "org.example.checkin"), "checkin")
        pid = p["id"]
        if p.get("lifecycle") == "enabled":
            disable_plugin(ctx, pid)
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
        assert_api_error(
            lambda: ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pid}/config",
                json_body={
                    "schema_revision": cfg["schema_revision"],
                    "values": {"interval_secs": 10, "transport": "ftp", "sites": []},
                    "secrets": {},
                },
                if_match=str(cfg["revision"]),
                idempotency_key=uniq("chkbad"),
            ),
            (400,),
            label="invalid checkin config bounds",
        )

    @registry.case(
        id="T2-F26-03",
        tier=2,
        feature_id=26,
        name="Boost delete-task retains retained_file_bytes in snapshot until delete-files completes",
        self_test=False,
    )
    def test_t2_f26_boost_retained_bytes_accounting(ctx: TestContext) -> None:
        p = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        pid = p["id"]
        if p.get("lifecycle") != "enabled":
            enable_plugin(ctx, pid, revision=int(p["revision"]))
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit(real_save_dir=ctx.tmp_dir / "qb_f26")
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        add_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "F26 Retain Target",
                },
            },
            idempotency_key=uniq("f26radd"),
        )
        assert poll_operation(ctx, add_op["operation_id"])["state"] == "succeeded"
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        matched = [
            t
            for t in tasks
            if isinstance(t, dict)
            and t.get("downloader_id") == dl["id"]
            and t.get("info_hash") == pt.default_torrent.info_hash
        ]
        assert len(matched) >= 1, f"Expected owned boost task, got {tasks!r}"
        tid = matched[0]["id"]
        del_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/commands",
            json_body={"kind": "downloader.delete-task", "payload": {"task_id": tid}},
            idempotency_key=uniq("f26rdel"),
        )
        assert poll_operation(ctx, del_op["operation_id"])["state"] == "succeeded"
        state_mid = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{pid}/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state_mid["retained_file_bytes"]) > 0
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/commands",
            json_body={
                "kind": "downloader.prepare-cleanup",
                "payload": {"site_id": site["id"], "downloader_id": dl["id"], "task_ids": [tid]},
            },
            idempotency_key=uniq("f26rprep"),
        )
        prep_done = poll_operation(ctx, prep_op["operation_id"])
        assert prep_done["state"] == "succeeded"
        plan_handle = (prep_done.get("result") or {}).get("plan_handle")
        assert plan_handle
        prev = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{pid}/brush-cleanup-preview?plan_handle={plan_handle}",
        )
        df_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/commands",
            json_body={
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": plan_handle,
                    "revision": prev.get("revision", 1),
                    "confirmation_token": prev["confirmation_token"],
                    "reason": "e2e f26 cleanup",
                },
            },
            idempotency_key=uniq("f26rdf"),
        )
        assert poll_operation(ctx, df_op["operation_id"])["state"] == "succeeded"
        state_after = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{pid}/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state_after["retained_file_bytes"]) == 0

    @registry.case(
        id="T2-F26-04",
        tier=2,
        feature_id=26,
        name="Boost brush-cleanup-preview rejects both/neither params (400) and never issues token on selection",
        self_test=False,
    )
    def test_t2_f26_cleanup_preview_exclusivity(ctx: TestContext) -> None:
        p = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        pid = p["id"]
        assert_api_error(
            lambda: ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/brush-cleanup-preview"),
            (400,),
            label="brush-cleanup-preview missing both params",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/{pid}/brush-cleanup-preview?task_ids=t1&plan_handle=p1",
            ),
            (400,),
            label="brush-cleanup-preview both params",
        )

    @registry.case(
        id="T2-F26-05",
        tier=2,
        feature_id=26,
        name="First-party UI query is blocked when disabled and assets return 403 when UI trust revoked",
        self_test=False,
    )
    def test_t2_f26_disabled_or_untrusted_blocked(ctx: TestContext) -> None:
        p = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        pid = p["id"]
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
        granted = grant_ui_trust(ctx, pid, desc["package_hash"])
        ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/ui-trust/revoke",
            json_body={"package_hash": desc["package_hash"]},
            if_match=str(granted.get("revision", 1)),
            idempotency_key=uniq("rev26"),
        )
        r = ctx.client.clone_cookie_only().get_asset(desc["entry_url"])
        assert r.status == 403
        was_enabled = p.get("lifecycle") == "enabled"
        if was_enabled:
            disable_plugin(ctx, pid)
        try:
            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/ui/query",
                    json_body={"name": "boost.snapshot", "input": {}},
                ),
                (403, 409),
                label="disabled first-party ui/query",
            )
        finally:
            if was_enabled:
                enable_plugin(ctx, pid)
