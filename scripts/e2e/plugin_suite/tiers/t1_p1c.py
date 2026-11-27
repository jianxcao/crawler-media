"""Tier 1 Happy-Path Tests for Features #1-#8 (P1c Package, Lifecycle & Admin APIs)."""
from __future__ import annotations

import uuid
import zipfile

from ..lib.client import ApiError
from ..lib.fixtures import (
    build_cmplugin,
    build_secret_config_schema,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
)
from . import SuiteRegistry, TestContext
from ._helpers import (
    assert_api_error,
    disable_plugin,
    enable_plugin,
    extract_prep_grants,
    grant_ui_trust,
    obtain_reauth,
    pin_trust_key,
    poll_operation,
    prepare_and_install_plugin,
    restart_and_relogin,
    set_developer_mode,
    setup_signed_plugin_ready,
    uniq,
    upload_cmplugin,
)


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #1: Shared .cmplugin Archive Validator (5 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F01-01",
        tier=1,
        feature_id=1,
        name="Valid hook-plugin .cmplugin (Deflate + Store) uploads with matching SHA-256",
        self_test=False,
    )
    def test_f01_hook_upload(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x01" * 32)
        pin_trust_key(ctx, kp, label=uniq("f01"))
        pkg = build_cmplugin(
            plugin_id=f"org.example.{uniq('f01hook')}",
            kind="hook",
            compress_overrides={"LICENSE": zipfile.ZIP_STORED},
        )
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("hash") == pkg.sha256_hex
        assert uploaded.get("upload_id")

    @registry.case(
        id="T1-F01-02",
        tier=1,
        feature_id=1,
        name="Valid task-plugin .cmplugin uploads and prepares successfully",
        self_test=False,
    )
    def test_f01_task_prepare(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x02" * 32)
        pin_trust_key(ctx, kp, label=uniq("f01task"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('f01task')}", kind="task")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("hash") == pkg.sha256_hex
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": uploaded["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        done = poll_operation(ctx, prep_op["operation_id"])
        assert done["state"] == "succeeded"
        prep = done.get("result") or done
        assert prep.get("package_hash") == pkg.sha256_hex

    @registry.case(
        id="T1-F01-03",
        tier=1,
        feature_id=1,
        name="Valid endpoint-plugin .cmplugin uploads and prepares successfully",
        self_test=False,
    )
    def test_f01_endpoint_prepare(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x03" * 32)
        pin_trust_key(ctx, kp, label=uniq("f01ep"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('f01ep')}", kind="endpoint")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("hash") == pkg.sha256_hex
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": uploaded["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        done = poll_operation(ctx, prep_op["operation_id"])
        assert done["state"] == "succeeded"
        prep = done.get("result") or done
        assert prep.get("package_hash") == pkg.sha256_hex

    @registry.case(
        id="T1-F01-04",
        tier=1,
        feature_id=1,
        name="Valid yaml-only .cmplugin (no .wasm) uploads and prepares",
        self_test=False,
    )
    def test_f01_yaml_only_prepare(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x04" * 32)
        pin_trust_key(ctx, kp, label=uniq("f01yaml"))
        pkg = build_cmplugin(
            plugin_id=f"org.example.{uniq('f01yaml')}",
            kind="yaml-only",
            site_id=uniq("site").replace("_", "-"),
        )
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("hash") == pkg.sha256_hex
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": uploaded["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        done = poll_operation(ctx, prep_op["operation_id"])
        assert done["state"] == "succeeded"
        prep = done.get("result") or done
        assert prep.get("package_hash") == pkg.sha256_hex

    @registry.case(
        id="T1-F01-05",
        tier=1,
        feature_id=1,
        name="Valid ui-only .cmplugin uploads and prepares",
        self_test=False,
    )
    def test_f01_ui_only_prepare(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x05" * 32)
        pin_trust_key(ctx, kp, label=uniq("f01ui"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('f01ui')}", kind="ui-only")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("hash") == pkg.sha256_hex
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": uploaded["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        done = poll_operation(ctx, prep_op["operation_id"])
        assert done["state"] == "succeeded"
        prep = done.get("result") or done
        assert prep.get("package_hash") == pkg.sha256_hex

    # -----------------------------------------------------------------------
    # Feature #2: Minisign Offline Signature & Dev-Mode Policy (5 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F02-01",
        tier=1,
        feature_id=2,
        name="Prehashed ED minisign signature verifies offline and activates",
        self_test=False,
    )
    def test_f02_ed_prehashed(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x11" * 32)
        pin_trust_key(ctx, kp, label=uniq("ed-prehash"))
        pid = f"org.example.{uniq('edpre')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("signature_status") in ("signed", "trusted", "verified")
        plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, sig_text=sig, pin_key=False)
        enabled = enable_plugin(ctx, pid, revision=int(plugin["revision"]))
        assert enabled["lifecycle"] == "enabled"
        assert int(enabled.get("generation", 0)) >= 1

    @registry.case(
        id="T1-F02-02",
        tier=1,
        feature_id=2,
        name="Legacy unhashed Ed minisign signature verifies and activates",
        self_test=False,
    )
    def test_f02_ed_legacy(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x12" * 32)
        pin_trust_key(ctx, kp, label=uniq("ed-legacy"))
        pid = f"org.example.{uniq('edleg')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=False)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("signature_status") in ("signed", "trusted", "verified")
        plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, sig_text=sig, pin_key=False)
        enabled = enable_plugin(ctx, pid, revision=int(plugin["revision"]))
        assert enabled["lifecycle"] == "enabled"
        assert int(enabled.get("generation", 0)) >= 1

    @registry.case(
        id="T1-F02-03",
        tier=1,
        feature_id=2,
        name="Multi-key trust store resolves matching active key among revoked and unrelated keys",
        self_test=False,
    )
    def test_f02_multikey_resolution(ctx: TestContext) -> None:
        kp_revoked = generate_minisign_keypair(seed=b"\x13" * 32)
        kp_other = generate_minisign_keypair(seed=b"\x14" * 32)
        kp_active = generate_minisign_keypair(seed=b"\x15" * 32)
        pinned_rev = pin_trust_key(ctx, kp_revoked, label=uniq("rev"))
        pin_trust_key(ctx, kp_other, label=uniq("other"))
        pin_trust_key(ctx, kp_active, label=uniq("active"))
        reauth = obtain_reauth(ctx, purpose="trust-key:revoke")
        rev_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugin-trust-keys/{pinned_rev['key_id']}/revoke",
            json_body={"reason": "test rotation", "reauth_token": reauth},
            if_match=str(pinned_rev["revision"]),
            idempotency_key=uniq("revkey"),
        )
        rev_done = poll_operation(ctx, rev_op["operation_id"])
        assert rev_done["state"] == "succeeded"
        keys = ctx.call_phase1("GET", "/api/v1/plugin-trust-keys")
        rev_rows = [k for k in keys if k.get("key_id") == pinned_rev["key_id"]]
        assert len(rev_rows) == 1 and rev_rows[0].get("status") == "revoked", (
            f"Expected revoked key {pinned_rev['key_id']} in {keys!r}"
        )
        pid = f"org.example.{uniq('mkey')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp_active, hashed=True)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("signature_status") in ("signed", "trusted", "verified"), (
            f"Expected signed/trusted/verified signature_status, got {uploaded!r}"
        )
        plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp_active, sig_text=sig, pin_key=False)
        enabled = enable_plugin(ctx, pid, revision=int(plugin["revision"]))
        assert enabled["lifecycle"] == "enabled"
        assert int(enabled.get("generation", 0)) >= 1

    @registry.case(
        id="T1-F02-04",
        tier=1,
        feature_id=2,
        name="Developer mode enable persists across restart",
        self_test=False,
    )
    def test_f02_devmode_persist(ctx: TestContext) -> None:
        res = set_developer_mode(ctx, True)
        try:
            dev_val = res["developer_mode"] if "developer_mode" in res else res.get("enabled")
            assert bool(dev_val) is True, f"Expected developer_mode=True, got {res!r}"
            assert int(res.get("revision", 0)) >= 1
            restart_and_relogin(ctx)
            after = ctx.call_phase1("GET", "/api/v1/settings/plugins")
            after_val = after["developer_mode"] if "developer_mode" in after else after.get("enabled")
            assert bool(after_val) is True, f"Expected persisted developer_mode=True, got {after!r}"
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T1-F02-05",
        tier=1,
        feature_id=2,
        name="Unsigned .cmplugin uploads, installs, and enables when developer mode is enabled",
        self_test=False,
    )
    def test_f02_unsigned_devmode(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            pid = f"org.example.{uniq('devunsig')}"
            pkg = build_cmplugin(plugin_id=pid, kind="hook")
            uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=None)
            assert uploaded.get("signature_status") in ("unsigned", "development")
            plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=None, sig_text=None, pin_key=False)
            enabled = enable_plugin(ctx, pid, revision=int(plugin["revision"]))
            assert enabled["lifecycle"] == "enabled"
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #3: Strict plugin.toml, Bounded JSON Schema & asset-manifest.json
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F03-01",
        tier=1,
        feature_id=3,
        name="Full valid plugin.toml with [limits], [[http]], and multiple authors passes upload/prepare",
        self_test=False,
    )
    def test_f03_full_toml(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x21" * 32)
        pin_trust_key(ctx, kp, label=uniq("f03toml"))
        pkg = build_cmplugin(
            plugin_id=f"org.example.{uniq('fulltoml')}",
            kind="hook",
            authors=("Alice Author", "Bob Maintainer"),
            http_rules=[
                {"origin": "https://api.example.com", "path_prefix": "/v1", "methods": ["GET", "POST"]}
            ],
            limits={
                "memory_mib": 32,
                "fuel_limit": 5000000,
                "io_ms": 1000,
                "wall_ms": 1500,
                "http_calls": 4,
                "response_bytes": 1048576,
                "kv_bytes": 524288,
            },
        )
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert uploaded.get("hash") == pkg.sha256_hex
        prep = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": uploaded["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        done = poll_operation(ctx, prep["operation_id"])
        assert done["state"] == "succeeded"
        prep_res = done.get("result") or done
        assert prep_res.get("package_hash") == pkg.sha256_hex

    @registry.case(
        id="T1-F03-02",
        tier=1,
        feature_id=3,
        name="Bounded JSON Schema with local $defs/$ref, pattern, constraints, and x-ui.widget installs",
        self_test=False,
    )
    def test_f03_bounded_schema(ctx: TestContext) -> None:
        schema = {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": False,
            "$defs": {
                "tag_item": {"type": "string", "pattern": "^[a-z0-9-]+$", "minLength": 1, "maxLength": 32}
            },
            "properties": {
                "mode": {"type": "string", "enum": ["fast", "safe"], "default": "safe"},
                "interval": {"type": "integer", "minimum": 60, "maximum": 3600, "default": 300, "x-ui": {"widget": "interval"}},
                "tags": {"type": "array", "minItems": 0, "maxItems": 5, "uniqueItems": True, "items": {"$ref": "#/$defs/tag_item"}},
            },
        }
        pkg, _, plugin = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('schema')}",
            schema_override=schema,
            enable=False,
        )
        got = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config-schema")
        assert isinstance(got, dict) and got.get("schema_revision") is not None
        assert "tag_item" in str(got) and "interval" in str(got)
        assert plugin["lifecycle"] == "installed-disabled"

    @registry.case(
        id="T1-F03-03",
        tier=1,
        feature_id=3,
        name="POST /config/validate returns 200 valid without mutating config_revision",
        self_test=False,
    )
    def test_f03_config_validate_readonly(ctx: TestContext) -> None:
        pkg, _, before = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('cfgval')}", enable=False
        )
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        res = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/config/validate",
            json_body={
                "schema_revision": cfg["schema_revision"],
                "values": {"dry_run": False, "threshold": 42},
            },
        )
        assert isinstance(res, dict) and res.get("valid") is True
        try:
            bad_res = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/config/validate",
                json_body={
                    "schema_revision": cfg["schema_revision"],
                    "values": {"dry_run": "not-a-bool", "threshold": -999, "unexpected_field": 1},
                },
            )
            assert isinstance(bad_res, dict) and bad_res.get("valid") is False, (
                f"Expected invalid config validation failure, got {bad_res!r}"
            )
        except ApiError as exc:
            assert exc.status in (400, 422), (
                f"Expected 400/422 on invalid config/validate, got {exc.status}: {exc.message}"
            )
        after = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}")
        assert after["config_revision"] == before["config_revision"]

    @registry.case(
        id="T1-F03-04",
        tier=1,
        feature_id=3,
        name="Secret config field supports replace, keep, and clear without plaintext exposure",
        self_test=False,
    )
    def test_f03_secret_lifecycle(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('sec')}",
            schema_override=build_secret_config_schema(),
            enable=False,
        )
        cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        srev = cfg0["schema_revision"]
        c1 = ctx.call_phase1(
            "PUT",
            f"/api/v1/plugins/{pkg.plugin_id}/config",
            json_body={
                "schema_revision": srev,
                "values": {"profile_label": "demo", "dry_run": True, "threshold": 10},
                "secrets": {"api_token": {"action": "replace", "value": "top-secret-xyz"}},
            },
            if_match=str(cfg0["revision"]),
            idempotency_key=uniq("sec-rep"),
        )
        assert c1["secrets"]["api_token"]["configured"] is True
        assert "top-secret-xyz" not in str(c1)
        c2 = ctx.call_phase1(
            "PUT",
            f"/api/v1/plugins/{pkg.plugin_id}/config",
            json_body={
                "schema_revision": srev,
                "values": {"profile_label": "demo", "dry_run": False, "threshold": 20},
                "secrets": {"api_token": {"action": "keep"}},
            },
            if_match=str(c1["revision"]),
            idempotency_key=uniq("sec-keep"),
        )
        assert c2["secrets"]["api_token"]["configured"] is True
        c3 = ctx.call_phase1(
            "PUT",
            f"/api/v1/plugins/{pkg.plugin_id}/config",
            json_body={
                "schema_revision": srev,
                "values": {"profile_label": "demo", "dry_run": False, "threshold": 20},
                "secrets": {"api_token": {"action": "clear"}},
            },
            if_match=str(c2["revision"]),
            idempotency_key=uniq("sec-clr"),
        )
        assert c3["secrets"]["api_token"]["configured"] is False

    @registry.case(
        id="T1-F03-05",
        tier=1,
        feature_id=3,
        name="Multi-surface UI and .js/.mjs/.css/.png/.woff2 assets pass validation",
        self_test=False,
    )
    def test_f03_ui_multi_asset(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x25" * 32)
        pin_trust_key(ctx, kp, label=uniq("f03ui"))
        pid = f"org.example.{uniq('uimix')}"
        pkg = build_cmplugin(
            plugin_id=pid,
            kind="ui-only",
            ui_overrides={
                "surfaces": [
                    {"id": "cfg", "kind": "config", "expose": "./Config", "title": "Config", "icon": "sliders"},
                    {"id": "pg", "kind": "page", "expose": "./Page", "title": "Page", "icon": "file-text"},
                    {"id": "app", "kind": "app", "expose": "./App", "title": "App", "icon": "calendar-check"},
                    {"id": "dash", "kind": "dashboard", "expose": "./Dashboard", "title": "Dash", "icon": "activity"},
                ]
            },
            extra_ui_assets={
                "ui/assets/chunk.mjs": (b"export const x = 1;\n", "application/javascript"),
                "ui/assets/icon.png": (b"\x89PNG\r\n\x1a\n", "image/png"),
                "ui/assets/font.woff2": (b"wOF2", "font/woff2"),
            },
        )
        plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, pin_key=False)
        assert plugin["id"] == pid
        assert plugin["lifecycle"] == "installed-disabled"
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
        assert desc.get("plugin_id") == pid
        assert desc.get("package_hash") == pkg.sha256_hex
        assert {s.get("id"): s.get("kind") for s in (desc.get("surfaces") or [])} == {
            "cfg": "config",
            "pg": "page",
            "app": "app",
            "dash": "dashboard",
        }
        grant_ui_trust(ctx, pid, pkg.sha256_hex)
        cookie_cli = ctx.client.clone_cookie_only()
        for rel_path, expected_bytes in (
            ("ui/assets/chunk.mjs", b"export const x = 1;\n"),
            ("ui/assets/icon.png", b"\x89PNG\r\n\x1a\n"),
            ("ui/assets/font.woff2", b"wOF2"),
        ):
            r = cookie_cli.get_asset(
                f"/api/v1/plugin-ui-assets/{pid}/{pkg.sha256_hex}/{rel_path}",
                use_cookie=True,
                no_bearer=True,
            )
            assert r.status == 200, f"Expected 200 fetching {rel_path}, got {r.status}"
            assert r.body == expected_bytes, f"Mismatch for {rel_path}: {r.body!r}"

    # -----------------------------------------------------------------------
    # Feature #4: plugin_trust_keys DDL & Trust Key Admin APIs (5 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F04-01",
        tier=1,
        feature_id=4,
        name="POST /plugin-trust-keys/prepare computes key_id and fingerprint offline",
        self_test=False,
    )
    def test_f04_prepare(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x31" * 32)
        prep = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-trust-keys/prepare",
            json_body={"public_key": kp.public_key_text, "label": uniq("prep")},
        )
        assert prep["key_id"].upper() == kp.key_id_hex
        assert prep["fingerprint"] == kp.fingerprint
        assert prep.get("consent_token")
        assert prep.get("expires_at")

    @registry.case(
        id="T1-F04-02",
        tier=1,
        feature_id=4,
        name="POST /plugin-trust-keys pins key with revision 1 and persists across restart",
        self_test=False,
    )
    def test_f04_pin_persists(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned = pin_trust_key(ctx, kp, label=uniq("persist"))
        assert pinned["status"] == "trusted"
        assert int(pinned["revision"]) == 1
        restart_and_relogin(ctx)
        listed = ctx.call_phase1("GET", "/api/v1/plugin-trust-keys")
        assert any(k["key_id"] == pinned["key_id"] and k["status"] == "trusted" for k in listed)

    @registry.case(
        id="T1-F04-03",
        tier=1,
        feature_id=4,
        name="GET /plugin-trust-keys reports cursor pagination and associated_package_count",
        self_test=False,
    )
    def test_f04_list_and_package_count(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned = pin_trust_key(ctx, kp, label=uniq("cnt"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('kcnt')}", kind="hook")
        prepare_and_install_plugin(ctx, pkg, kp=kp, pin_key=False)
        resp, env = ctx.client.request_envelope("GET", "/api/v1/plugin-trust-keys?limit=50")
        assert resp.status == 200 and "next_cursor" in env
        matched = [k for k in env["data"] if k["key_id"] == pinned["key_id"]]
        assert len(matched) == 1
        assert int(matched[0]["associated_package_count"]) >= 1

    @registry.case(
        id="T1-F04-04",
        tier=1,
        feature_id=4,
        name="POST /plugin-trust-keys/:keyId/revoke transitions key to revoked with metadata",
        self_test=False,
    )
    def test_f04_revoke(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned = pin_trust_key(ctx, kp, label=uniq("rev"))
        reauth = obtain_reauth(ctx, purpose="trust-key:revoke")
        op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugin-trust-keys/{pinned['key_id']}/revoke",
            json_body={"reason": "compromised", "reauth_token": reauth},
            if_match=str(pinned["revision"]),
            idempotency_key=uniq("rev"),
        )
        done = poll_operation(ctx, op["operation_id"])
        assert done["state"] == "succeeded"
        keys = ctx.call_phase1("GET", "/api/v1/plugin-trust-keys")
        revoked = [k for k in keys if k["key_id"] == pinned["key_id"]][0]
        assert revoked["status"] == "revoked"
        assert int(revoked["revision"]) == 2
        assert revoked.get("revoked_at") and revoked.get("revoked_reason") == "compromised"

    @registry.case(
        id="T1-F04-05",
        tier=1,
        feature_id=4,
        name="Revoking trust key automatically drains/disables active dependent plugin",
        self_test=False,
    )
    def test_f04_revoke_drains_plugin(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned = pin_trust_key(ctx, kp, label=uniq("drain"))
        pid = f"org.example.{uniq('kdrain')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, pin_key=False)
        enable_plugin(ctx, pid, revision=int(plugin["revision"]))
        reauth = obtain_reauth(ctx, purpose="trust-key:revoke")
        op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugin-trust-keys/{pinned['key_id']}/revoke",
            json_body={"reason": "drain dependent", "reauth_token": reauth},
            if_match=str(pinned["revision"]),
            idempotency_key=uniq("revdrain"),
        )
        done = poll_operation(ctx, op["operation_id"])
        assert done["state"] == "succeeded"
        after = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
        assert after["lifecycle"] == "disabled"

    # -----------------------------------------------------------------------
    # Feature #5: Staged Install & Crash Recovery (5 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F05-01",
        tier=1,
        feature_id=5,
        name="POST /plugin-installations/prepare resolves Preparation with consent_token and grant_diff",
        self_test=False,
    )
    def test_f05_prepare_result(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("f05p"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('f05p')}", kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up["upload_id"]},
            if_match="0",
            idempotency_key=uniq("f05prep"),
        )
        done = poll_operation(ctx, op["operation_id"])
        assert done["state"] == "succeeded"
        res = done.get("result") or done
        assert res.get("preparation_id") and res.get("consent_token")
        assert res.get("package_hash") == pkg.sha256_hex

    @registry.case(
        id="T1-F05-02",
        tier=1,
        feature_id=5,
        name="POST /plugin-installations transitions plugin to installed-disabled revision 1 generation 0",
        self_test=False,
    )
    def test_f05_install_state(ctx: TestContext) -> None:
        pkg, _, plugin = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f05i')}", enable=False
        )
        assert plugin["id"] == pkg.plugin_id
        assert plugin["lifecycle"] == "installed-disabled"
        assert int(plugin["revision"]) == 1
        assert int(plugin["generation"]) == 0

    @registry.case(
        id="T1-F05-03",
        tier=1,
        feature_id=5,
        name="Idempotent replay of POST /plugin-installations returns original operation_id after completion",
        self_test=False,
    )
    def test_f05_idempotent_replay(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("f05idem"))
        pid = f"org.example.{uniq('f05idem')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        grants = extract_prep_grants(prep)
        assert len(grants) >= 1, f"Expected non-empty grants in prep: {prep!r}"
        idem_key = uniq("inst-replay")
        body = {
            "preparation_id": prep["preparation_id"],
            "consent_token": prep["consent_token"],
            "grants": grants,
        }
        op1 = ctx.call_phase1(
            "POST", "/api/v1/plugin-installations", json_body=body, if_match="0", idempotency_key=idem_key
        )
        assert poll_operation(ctx, op1["operation_id"])["state"] == "succeeded"
        op2 = ctx.call_phase1(
            "POST", "/api/v1/plugin-installations", json_body=body, if_match="0", idempotency_key=idem_key
        )
        assert op2["operation_id"] == op1["operation_id"]

    @registry.case(
        id="T1-F05-04",
        tier=1,
        feature_id=5,
        name="SIGKILL during install followed by restart recovers original operation cleanly",
        self_test=False,
    )
    def test_f05_crash_recovery(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("f05kill"))
        pid = f"org.example.{uniq('f05kill')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        grants = extract_prep_grants(prep)
        assert len(grants) >= 1, f"Expected non-empty grants in prep: {prep!r}"
        inst_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations",
            json_body={
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": grants,
            },
            if_match="0",
            idempotency_key=uniq("instkill"),
        )
        restart_and_relogin(ctx, kill=True)
        op_after = ctx.call_phase1("GET", f"/api/v1/plugin-operations/{inst_op['operation_id']}")
        if op_after.get("state") not in ("succeeded", "failed", "cancelled"):
            assert "revision" in op_after, f"Missing revision on operation: {op_after!r}"
            try:
                ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugin-operations/{inst_op['operation_id']}/resume",
                    json_body={},
                    if_match=str(op_after["revision"]),
                    idempotency_key=uniq("f05res"),
                )
            except ApiError as exc:
                if exc.status != 409:
                    raise
        done = poll_operation(ctx, inst_op["operation_id"])
        assert done["state"] == "succeeded"
        plugin = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
        assert plugin["lifecycle"] == "installed-disabled"

    @registry.case(
        id="T1-F05-05",
        tier=1,
        feature_id=5,
        name="POST /plugin-operations/:id/cancel transitions queued op to cancelled and releases lock",
        self_test=False,
    )
    def test_f05_cancel_releases_lock(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("f05can"))
        pid = f"org.example.{uniq('f05can')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        try:
            can_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugin-operations/{prep_op['operation_id']}/cancel",
                json_body={"reason": "cancel test"},
                if_match=str(prep_op.get("revision", 1)),
                idempotency_key=uniq("cancel"),
            )
            assert can_op.get("operation_id") == prep_op["operation_id"]
            if can_op.get("state") in ("cancelling", "cancelled"):
                done = poll_operation(ctx, prep_op["operation_id"])
                assert done["state"] == "cancelled", (
                    f"Cancel returned {can_op.get('state')!r}, expected terminal 'cancelled', got {done!r}"
                )
            else:
                done = poll_operation(ctx, prep_op["operation_id"])
                assert done["state"] in ("cancelled", "succeeded"), (
                    f"Unexpected terminal state after cancel: {done!r}"
                )
                if done["state"] == "succeeded":
                    assert_api_error(
                        lambda: ctx.call_phase1(
                            "POST",
                            f"/api/v1/plugin-operations/{done['operation_id']}/cancel",
                            json_body={"reason": "cancel after succeeded"},
                            if_match=str(done.get("revision", prep_op.get("revision", 1))),
                            idempotency_key=uniq("canafter"),
                        ),
                        (409,),
                        label="cancel on already-succeeded operation",
                    )
        except ApiError as exc:
            if exc.status != 409:
                raise
            prep_done = poll_operation(ctx, prep_op["operation_id"])
            assert prep_done["state"] == "succeeded"
            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugin-operations/{prep_done['operation_id']}/cancel",
                    json_body={"reason": "cancel terminal"},
                    if_match=str(prep_done.get("revision", 1)),
                    idempotency_key=uniq("canterm"),
                ),
                (409,),
                label="cancel on already-succeeded operation",
            )
        # Subsequent install on same plugin_id must succeed without stale lock
        plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, sig_text=sig, pin_key=False)
        assert plugin["id"] == pid

    # -----------------------------------------------------------------------
    # Feature #6: Lifecycle State Machine (5 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F06-01",
        tier=1,
        feature_id=6,
        name="POST /plugins/:id/enable transitions installed-disabled -> enabled and increments generation",
        self_test=False,
    )
    def test_f06_enable(ctx: TestContext) -> None:
        pkg, _, plugin = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06en')}", enable=False
        )
        enabled = enable_plugin(ctx, pkg.plugin_id, revision=int(plugin["revision"]))
        assert enabled["lifecycle"] == "enabled"
        assert int(enabled["generation"]) >= 1
        assert int(enabled["revision"]) > int(plugin["revision"])

    @registry.case(
        id="T1-F06-02",
        tier=1,
        feature_id=6,
        name="POST /plugins/:id/disable transitions enabled -> disabled and increments generation",
        self_test=False,
    )
    def test_f06_disable(ctx: TestContext) -> None:
        pkg, _, enabled = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06dis')}", enable=True
        )
        disabled = disable_plugin(ctx, pkg.plugin_id)
        assert disabled["lifecycle"] == "disabled"
        assert int(disabled["generation"]) > int(enabled["generation"])

    @registry.case(
        id="T1-F06-03",
        tier=1,
        feature_id=6,
        name="Re-enable increments generation and permissions/grant updates effective_grants",
        self_test=False,
    )
    def test_f06_reenable_and_grant(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("f06re"))
        pid = f"org.example.{uniq('f06re')}"
        pkg = build_cmplugin(
            plugin_id=pid,
            kind="hook",
            hook_permissions=["hooks:search.after", "kv"],
        )
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up["upload_id"]},
            if_match="0",
            idempotency_key=uniq("prep"),
        )
        prep_done = poll_operation(ctx, prep_op["operation_id"])
        assert prep_done["state"] == "succeeded"
        prep = prep_done.get("result") or prep_done
        all_grants = extract_prep_grants(prep)
        hook_grants = [g for g in all_grants if "hooks:search.after" in str(g)]
        kv_grants = [g for g in all_grants if "kv" in str(g) and "hooks:" not in str(g)]
        assert hook_grants and kv_grants, f"Expected hook+kv grants in {all_grants!r}"

        inst_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations",
            json_body={
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": hook_grants,
            },
            if_match="0",
            idempotency_key=uniq("inst"),
        )
        assert poll_operation(ctx, inst_op["operation_id"])["state"] == "succeeded"
        enabled = enable_plugin(ctx, pid)
        disabled = disable_plugin(ctx, pid)

        prep2 = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/permissions/prepare",
            json_body={"requested_grants": all_grants},
        )
        if isinstance(prep2.get("grant_diff"), dict):
            prep2_grants = extract_prep_grants(prep2)
            assert len(prep2_grants) == len(all_grants), (
                f"Expected added+kept grants to match all_grants={all_grants!r}, got {prep2['grant_diff']!r}"
            )
        grant_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/permissions/grant",
            json_body={
                "consent_token": prep2["consent_token"],
                "grants": all_grants,
            },
            if_match=str(disabled["revision"]),
            idempotency_key=uniq("grant"),
        )
        assert poll_operation(ctx, grant_op["operation_id"])["state"] == "succeeded"
        perms = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/permissions")
        assert "kv" in str(perms)
        re_enabled = enable_plugin(ctx, pid)
        assert int(re_enabled["generation"]) > int(enabled["generation"])

    @registry.case(
        id="T1-F06-04",
        tier=1,
        feature_id=6,
        name="POST /plugins/:id/permissions/revoke on enabled plugin drains to disabled and bumps generation",
        self_test=False,
    )
    def test_f06_revoke_permission(ctx: TestContext) -> None:
        pkg, _, enabled = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06rev')}", enable=True
        )
        perms = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/permissions")
        granted = (
            perms.get("effective_grants") or perms.get("grants") or []
            if isinstance(perms, dict)
            else (perms if isinstance(perms, list) else [])
        )
        kv_grants = [g for g in granted if "kv" in str(g) and "hooks:" not in str(g)]
        assert kv_grants, f"Expected active kv grant in {perms!r}"
        op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/permissions/revoke",
            json_body={"grants": kv_grants, "reason": "least privilege"},
            if_match=str(enabled["revision"]),
            idempotency_key=uniq("revperm"),
        )
        assert poll_operation(ctx, op["operation_id"])["state"] == "succeeded"
        after = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}")
        assert after["lifecycle"] == "disabled"
        assert int(after["generation"]) > int(enabled["generation"])

    @registry.case(
        id="T1-F06-05",
        tier=1,
        feature_id=6,
        name="Uninstall removes plugin from list while preserving audit history and optional private data",
        self_test=False,
    )
    def test_f06_uninstall(ctx: TestContext) -> None:
        pkg, kp, enabled = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06un')}", enable=True
        )
        disabled = disable_plugin(ctx, pkg.plugin_id)
        listed_before = ctx.call_phase1("GET", "/api/v1/plugins")
        assert any(p["id"] == pkg.plugin_id for p in listed_before), (
            f"Expected {pkg.plugin_id} in plugins list before uninstall, got {listed_before!r}"
        )
        un_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/uninstall",
            json_body={"remove_private_data": False},
            if_match=str(disabled["revision"]),
            idempotency_key=uniq("uninst"),
        )
        assert poll_operation(ctx, un_op["operation_id"])["state"] == "succeeded"
        listed = ctx.call_phase1("GET", "/api/v1/plugins")
        assert not any(p["id"] == pkg.plugin_id for p in listed)
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and len(audit) >= 1, (
            f"Expected preserved audit history after uninstall, got {audit!r}"
        )
        # Reinstall with remove_private_data=true afterward
        re_plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, pin_key=False)
        un_op2 = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/uninstall",
            json_body={"remove_private_data": True},
            if_match=str(re_plugin["revision"]),
            idempotency_key=uniq("uninst2"),
        )
        assert poll_operation(ctx, un_op2["operation_id"])["state"] == "succeeded"

    # -----------------------------------------------------------------------
    # Feature #7: Upgrade & Rollback (5 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F07-01",
        tier=1,
        feature_id=7,
        name="Upgrade v1->v2 with identical state_schema=1 succeeds using state_policy=preserve",
        self_test=False,
    )
    def test_f07_upgrade_preserve(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('uppres')}"
        pkg1, kp, p1 = setup_signed_plugin_ready(ctx, plugin_id=pid, version="1.0.0", state_schema=1, enable=False)
        pkg2 = build_cmplugin(plugin_id=pid, version="1.1.0", state_schema=1)
        sig2 = sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sig2)
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"upload_id": up2["upload_id"], "state_policy": "preserve", "target_config": {}},
            if_match=str(p1["revision"]),
            idempotency_key=uniq("upprep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        grants = extract_prep_grants(prep)
        assert len(grants) >= 1, f"Expected non-empty grants in prep: {prep!r}"
        up_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrade",
            json_body={
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": grants,
                "state_policy": "preserve",
                "target_config_digest": prep.get("target_config_digest", ""),
            },
            if_match=str(p1["revision"]),
            idempotency_key=uniq("upexec"),
        )
        assert poll_operation(ctx, up_op["operation_id"])["state"] == "succeeded"
        p2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
        assert p2["version"] == "1.1.0"
        assert int(p2["generation"]) > int(p1["generation"])

    @registry.case(
        id="T1-F07-02",
        tier=1,
        feature_id=7,
        name="Upgrade across state_schema 1->2 succeeds with state_policy=reset and reauth_token",
        self_test=False,
    )
    def test_f07_upgrade_reset(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('upreset')}"
        pkg1, kp, p1 = setup_signed_plugin_ready(ctx, plugin_id=pid, version="1.0.0", state_schema=1, enable=False)
        pkg2 = build_cmplugin(plugin_id=pid, version="2.0.0", state_schema=2)
        sig2 = sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sig2)
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
        target_cfg = {
            "schema_revision": cfg["schema_revision"],
            "values": {"dry_run": False, "threshold": 55},
            "secrets": {},
        }
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"upload_id": up2["upload_id"], "state_policy": "reset", "target_config": target_cfg},
            if_match=str(p1["revision"]),
            idempotency_key=uniq("upprep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        grants = extract_prep_grants(prep)
        assert len(grants) >= 1, f"Expected non-empty grants in prep: {prep!r}"
        reauth = obtain_reauth(ctx, purpose="plugin:upgrade")
        up_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrade",
            json_body={
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": grants,
                "state_policy": "reset",
                "target_config_digest": prep.get("target_config_digest", ""),
                "reauth_token": reauth,
            },
            if_match=str(p1["revision"]),
            idempotency_key=uniq("upexec"),
        )
        assert poll_operation(ctx, up_op["operation_id"])["state"] == "succeeded"
        p2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
        assert p2["version"] == "2.0.0"
        assert int(p2["generation"]) > int(p1["generation"])

    @registry.case(
        id="T1-F07-03",
        tier=1,
        feature_id=7,
        name="Rollback from v1.1.0 to v1.0.0 switches active package hash back with bumped generation",
        self_test=False,
    )
    def test_f07_rollback(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('rb')}"
        pkg1, kp, p1 = setup_signed_plugin_ready(ctx, plugin_id=pid, version="1.0.0", state_schema=1, enable=False)
        pkg2 = build_cmplugin(plugin_id=pid, version="1.1.0", state_schema=1)
        sig2 = sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sig2)
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"upload_id": up2["upload_id"], "state_policy": "preserve", "target_config": {}},
            if_match=str(p1["revision"]),
            idempotency_key=uniq("upprep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        grants = extract_prep_grants(prep)
        assert len(grants) >= 1, f"Expected non-empty grants in prep: {prep!r}"
        up_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrade",
            json_body={
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": grants,
                "state_policy": "preserve",
                "target_config_digest": prep.get("target_config_digest", ""),
            },
            if_match=str(p1["revision"]),
            idempotency_key=uniq("upexec"),
        )
        assert poll_operation(ctx, up_op["operation_id"])["state"] == "succeeded"
        p2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
        rb_prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"version": "1.0.0", "state_policy": "preserve", "target_config": {}},
            if_match=str(p2["revision"]),
            idempotency_key=uniq("rbprep"),
        )
        rb_prep = (poll_operation(ctx, rb_prep_op["operation_id"]).get("result")) or {}
        rb_grants = extract_prep_grants(rb_prep) or grants
        rb_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/rollback",
            json_body={
                "package_hash": pkg1.sha256_hex,
                "preparation_id": rb_prep["preparation_id"],
                "consent_token": rb_prep["consent_token"],
                "grants": rb_grants,
                "state_policy": "preserve",
                "target_config_digest": rb_prep.get("target_config_digest", ""),
            },
            if_match=str(p2["revision"]),
            idempotency_key=uniq("rbexec"),
        )
        assert poll_operation(ctx, rb_op["operation_id"])["state"] == "succeeded"
        p_after = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
        assert p_after["version"] == "1.0.0"
        assert int(p_after["generation"]) > int(p2["generation"])

    @registry.case(
        id="T1-F07-04",
        tier=1,
        feature_id=7,
        name="Standalone state/reset/prepare and state/reset succeed while disabled",
        self_test=False,
    )
    def test_f07_state_reset(ctx: TestContext) -> None:
        pkg, _, p1 = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('stres')}", enable=False
        )
        prep = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/state/reset/prepare",
            json_body={"security_scopes": ["global"]},
        )
        assert "consent_token" in prep and "kv_entries" in prep
        reauth = obtain_reauth(ctx, purpose="plugin:state-reset")
        op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/state/reset",
            json_body={
                "security_scopes": ["global"],
                "consent_token": prep["consent_token"],
                "reauth_token": reauth,
            },
            if_match=str(p1["revision"]),
            idempotency_key=uniq("stres"),
        )
        assert poll_operation(ctx, op["operation_id"])["state"] == "succeeded"

    @registry.case(
        id="T1-F07-05",
        tier=1,
        feature_id=7,
        name="Upgrade preparation computes grant_diff and config_diff accurately",
        self_test=False,
    )
    def test_f07_upgrade_diff(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('updiff')}"
        _, kp, p1 = setup_signed_plugin_ready(
            ctx,
            plugin_id=pid,
            version="1.0.0",
            hooks=[{"id": "search.after", "priority": 400}],
            hook_permissions=["hooks:search.after"],
            enable=False,
        )
        pkg2 = build_cmplugin(
            plugin_id=pid,
            version="1.1.0",
            hooks=[{"id": "search.after", "priority": 400}],
            hook_permissions=["hooks:search.after", "kv"],
        )
        sig2 = sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sig2)
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"upload_id": up2["upload_id"], "state_policy": "preserve", "target_config": {}},
            if_match=str(p1["revision"]),
            idempotency_key=uniq("updiff"),
        )
        done = poll_operation(ctx, prep_op["operation_id"])
        assert done["state"] == "succeeded"
        prep = done.get("result") or done
        assert "grant_diff" in prep and "config_diff" in prep
        assert "kv" in str(prep["grant_diff"])

    # -----------------------------------------------------------------------
    # Feature #8: Core Admin Plugin APIs & /plugin-reauth (5 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F08-01",
        tier=1,
        feature_id=8,
        name="POST /plugin-reauth issues purpose-bound token without mutating resource revision",
        self_test=False,
    )
    def test_f08_reauth(ctx: TestContext) -> None:
        settings_before = ctx.call_phase1("GET", "/api/v1/settings/plugins")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-reauth",
                json_body={"password": "wrong-password-for-f08", "purpose": "plugin:upload"},
            ),
            (401,),
            label="wrong password on /plugin-reauth",
        )
        res = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-reauth",
            json_body={"password": ctx.admin_password, "purpose": "plugin:upload"},
        )
        assert res.get("reauth_token") and res.get("purpose") == "plugin:upload"
        assert res.get("expires_at")
        settings_after = ctx.call_phase1("GET", "/api/v1/settings/plugins")
        assert settings_after["revision"] == settings_before["revision"]
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("f08re"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('f08re')}", kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        wrong_tok = obtain_reauth(ctx, purpose="trust-key:create")
        assert_api_error(
            lambda: upload_cmplugin(ctx, pkg.bytes, sig_text=sig, reauth_token=wrong_tok),
            (401, 403),
            label="wrong-purpose reauth_token on /plugin-uploads",
        )
        uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig, reauth_token=res["reauth_token"])
        assert uploaded.get("hash") == pkg.sha256_hex

    @registry.case(
        id="T1-F08-02",
        tier=1,
        feature_id=8,
        name="GET /plugins supports state filter and cursor/limit pagination",
        self_test=False,
    )
    def test_f08_list_pagination(ctx: TestContext) -> None:
        setup_signed_plugin_ready(ctx, plugin_id=f"org.example.{uniq('pg1')}", enable=False)
        setup_signed_plugin_ready(ctx, plugin_id=f"org.example.{uniq('pg2')}", enable=False)
        resp, env = ctx.client.request_envelope(
            "GET", "/api/v1/plugins?state=installed-disabled&limit=1"
        )
        assert resp.status == 200
        assert isinstance(env["data"], list) and len(env["data"]) == 1
        assert env.get("next_cursor"), f"Expected non-empty next_cursor with 2 matching plugins: {env!r}"
        _, env2 = ctx.client.request_envelope(
            "GET", f"/api/v1/plugins?state=installed-disabled&limit=1&cursor={env['next_cursor']}"
        )
        assert isinstance(env2["data"], list) and len(env2["data"]) == 1
        assert env2["data"][0]["id"] != env["data"][0]["id"]

    @registry.case(
        id="T1-F08-03",
        tier=1,
        feature_id=8,
        name="GET/PUT /plugins/:id/config updates values and redacts secrets",
        self_test=False,
    )
    def test_f08_config_crud(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f08cfg')}",
            schema_override=build_secret_config_schema(),
            enable=False,
        )
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        updated = ctx.call_phase1(
            "PUT",
            f"/api/v1/plugins/{pkg.plugin_id}/config",
            json_body={
                "schema_revision": cfg["schema_revision"],
                "values": {"profile_label": "alpha", "dry_run": False, "threshold": 77},
                "secrets": {"api_token": {"action": "replace", "value": "secret-val-888"}},
            },
            if_match=str(cfg["revision"]),
            idempotency_key=uniq("cfgput"),
        )
        assert int(updated["config_revision"]) > int(cfg["config_revision"])
        assert updated["secrets"]["api_token"]["configured"] is True
        assert "secret-val-888" not in str(updated)

    @registry.case(
        id="T1-F08-04",
        tier=1,
        feature_id=8,
        name="GET /plugins/:id/tasks and /tasks/:taskId/receipts return paginated summaries",
        self_test=False,
    )
    def test_f08_tasks_and_receipts(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f08tsk')}", kind="task", enable=True
        )
        tasks = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/tasks")
        assert isinstance(tasks, list) and len(tasks) >= 1
        tid = tasks[0]["id"]
        receipts = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/tasks/{tid}/receipts")
        assert isinstance(receipts, list)

    @registry.case(
        id="T1-F08-05",
        tier=1,
        feature_id=8,
        name="GET /plugins/:id/audit returns redacted admin events without exposing secrets",
        self_test=False,
    )
    def test_f08_audit_redacted(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f08aud')}",
            schema_override=build_secret_config_schema(),
            enable=False,
        )
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        updated = ctx.call_phase1(
            "PUT",
            f"/api/v1/plugins/{pkg.plugin_id}/config",
            json_body={
                "schema_revision": cfg["schema_revision"],
                "values": {"profile_label": "beta", "dry_run": True, "threshold": 5},
                "secrets": {"api_token": {"action": "replace", "value": "never-log-secret-999"}},
            },
            if_match=str(cfg["revision"]),
            idempotency_key=uniq("audsec"),
        )
        assert isinstance(updated, dict)
        assert int(updated["config_revision"]) > int(cfg["config_revision"])
        assert updated.get("values", {}).get("profile_label") == "beta"
        assert updated["secrets"]["api_token"]["configured"] is True
        assert "never-log-secret-999" not in str(updated)
        got_cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        assert isinstance(got_cfg, dict)
        assert got_cfg.get("values", {}).get("profile_label") == "beta"
        assert got_cfg.get("secrets", {}).get("api_token", {}).get("configured") is True
        assert "never-log-secret-999" not in str(got_cfg)
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and len(audit) >= 1
        assert any(
            isinstance(a, dict)
            and bool(a.get("operation_id"))
            and a.get("state") == "succeeded"
            and a.get("action") in ("install", "install-prepare", "enable", "config", "config-update")
            for a in audit
        ), f"Expected genuine operation audit entry in {audit!r}"
        assert "never-log-secret-999" not in str(audit)
