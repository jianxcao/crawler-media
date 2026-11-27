"""Tier 2 Boundary & Negative Tests for Features #1-#8 (P1c Package, Lifecycle & Admin APIs)."""
from __future__ import annotations

import json
import uuid

from ..lib.client import ApiError
from ..lib.fixtures import (
    build_cmplugin,
    build_negative_cmplugin,
    build_secret_config_schema,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
)
from . import SuiteRegistry, TestContext
from ._helpers import (
    assert_api_error,
    create_member_client,
    disable_plugin,
    extract_prep_grants,
    obtain_reauth,
    pin_trust_key,
    poll_operation,
    post_multipart_phase1,
    prepare_and_install_plugin,
    restart_and_relogin,
    set_developer_mode,
    setup_signed_plugin_ready,
    uniq,
    upload_cmplugin,
)


def _upload_negative_package(
    ctx: TestContext,
    raw_bytes: bytes,
    kp=None,
    expected_statuses=(400, 422),
) -> None:
    reauth = obtain_reauth(ctx, purpose="plugin:upload")
    uid = str(uuid.uuid4())
    files = [("file", "bad.cmplugin", raw_bytes, "application/zip")]
    if kp is not None:
        sig = sign_cmplugin_sha256(raw_bytes, kp, hashed=True)
        files.append(("signature", "bad.cmplugin.minisig", sig.encode("utf-8"), "text/plain"))
    assert_api_error(
        lambda: post_multipart_phase1(
            ctx.client,
            "/api/v1/plugin-uploads",
            fields={"upload_id": uid},
            files=files,
            reauth_token=reauth,
            idempotency_key=f"idem-{uid}",
            if_match="0",
        ),
        expected_statuses,
        label="upload negative .cmplugin",
    )


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #1: Shared .cmplugin Archive Validator (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F01-01",
        tier=2,
        feature_id=1,
        name="Archive path hazards (path_traversal, absolute_path) are rejected on upload",
        self_test=False,
    )
    def test_t2_f01_path_hazards(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in ("path_traversal", "absolute_path"):
                _upload_negative_package(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F01-02",
        tier=2,
        feature_id=1,
        name="Forbidden entry types & encryption (symlink, encrypted_entry, winzip_aes_encrypted) rejected",
        self_test=False,
    )
    def test_t2_f01_forbidden_types_and_encrypted(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in ("symlink", "encrypted_entry", "winzip_aes_encrypted"):
                _upload_negative_package(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F01-03",
        tier=2,
        feature_id=1,
        name="Duplicate paths, case collisions, CRC mismatch, and multi-disk/ZIP64 rejected",
        self_test=False,
    )
    def test_t2_f01_collisions_crc_zip64(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in ("duplicate_paths", "case_collision", "crc_mismatch", "multi_disk_or_zip64"):
                _upload_negative_package(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F01-04",
        tier=2,
        feature_id=1,
        name="Zip bomb (>100x compression ratio) rejected without decompressing full payload",
        self_test=False,
    )
    def test_t2_f01_zip_bomb(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            _upload_negative_package(ctx, build_negative_cmplugin("zip_bomb"))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F01-05",
        tier=2,
        feature_id=1,
        name="Undeclared executable, undeclared .wasm, and empty_package rejected",
        self_test=False,
    )
    def test_t2_f01_exec_and_empty(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in ("undeclared_executable", "undeclared_wasm", "empty_package"):
                _upload_negative_package(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #2: Minisign Offline Signature & Dev-Mode Policy (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F02-01",
        tier=2,
        feature_id=2,
        name="Normal mode (dev_mode=false) rejects unsigned .cmplugin",
        self_test=False,
    )
    def test_t2_f02_unsigned_rejected(ctx: TestContext) -> None:
        set_developer_mode(ctx, False)
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('unsig')}", kind="hook")
        _upload_negative_package(ctx, pkg.bytes, kp=None, expected_statuses=(400, 403, 422))

    @registry.case(
        id="T2-F02-02",
        tier=2,
        feature_id=2,
        name="Tampered package or tampered minisig comment rejected even when developer mode is enabled",
        self_test=False,
    )
    def test_t2_f02_tampered_sig_even_in_devmode(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x41" * 32)
        pin_trust_key(ctx, kp, label=uniq("t2f02"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('tamp')}", kind="hook")
        bad_sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True, tamper_comment=True)
        set_developer_mode(ctx, True)
        try:
            reauth = obtain_reauth(ctx, purpose="plugin:upload")
            uid = str(uuid.uuid4())
            assert_api_error(
                lambda: post_multipart_phase1(
                    ctx.client,
                    "/api/v1/plugin-uploads",
                    fields={"upload_id": uid},
                    files=[
                        ("file", "plugin.cmplugin", pkg.bytes, "application/zip"),
                        ("signature", "plugin.cmplugin.minisig", bad_sig.encode("utf-8"), "text/plain"),
                    ],
                    reauth_token=reauth,
                    idempotency_key=f"idem-{uid}",
                    if_match="0",
                ),
                (400, 403, 422),
                label="tampered signature in dev mode",
            )
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F02-03",
        tier=2,
        feature_id=2,
        name="Untrusted signer (including bundled publisher.pub inside ZIP) rejected",
        self_test=False,
    )
    def test_t2_f02_untrusted_and_bundled_pubkey(ctx: TestContext) -> None:
        set_developer_mode(ctx, False)
        kp_untrusted = generate_minisign_keypair(seed=b"\x42" * 32)
        pkg_bytes = build_negative_cmplugin("bundled_pubkey_zip")
        _upload_negative_package(ctx, pkg_bytes, kp=kp_untrusted, expected_statuses=(400, 403, 422))

    @registry.case(
        id="T2-F02-04",
        tier=2,
        feature_id=2,
        name="Revoked trust key blocks upload and blocks enable if revoked mid-install",
        self_test=False,
    )
    def test_t2_f02_revoked_key_blocks(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned = pin_trust_key(ctx, kp, label=uniq("revblk"))
        pid = f"org.example.{uniq('revblk')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, sig_text=sig, pin_key=False)
        reauth = obtain_reauth(ctx, purpose="trust-key:revoke")
        rev_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugin-trust-keys/{pinned['key_id']}/revoke",
            json_body={"reason": "revoked before enable", "reauth_token": reauth},
            if_match=str(pinned["revision"]),
            idempotency_key=uniq("revblk"),
        )
        rev_done = poll_operation(ctx, rev_op["operation_id"])
        assert rev_done["state"] == "succeeded"
        _upload_negative_package(ctx, pkg.bytes, kp=kp, expected_statuses=(400, 403, 409, 422))
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/enable",
                json_body={},
                if_match=str(plugin["revision"]),
                idempotency_key=uniq("enrev"),
            ),
            (400, 403, 409),
            label="enable with revoked signing key",
        )

    @registry.case(
        id="T2-F02-05",
        tier=2,
        feature_id=2,
        name="Disabling developer mode drains/disables unsigned active plugins",
        self_test=False,
    )
    def test_t2_f02_disable_devmode_drains_unsigned(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            pid = f"org.example.{uniq('devdr')}"
            pkg = build_cmplugin(plugin_id=pid, kind="hook")
            plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=None, sig_text=None, pin_key=False)
            en_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/enable",
                json_body={},
                if_match=str(plugin["revision"]),
                idempotency_key=uniq("endev"),
            )
            en_done = poll_operation(ctx, en_op["operation_id"])
            assert en_done["state"] == "succeeded"
            set_developer_mode(ctx, False)
            after = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
            assert after["lifecycle"] == "disabled"
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #3: Strict plugin.toml, Bounded JSON Schema & asset-manifest.json
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F03-01",
        tier=2,
        feature_id=3,
        name="Unknown TOML field, reserved plugin_id, and HTML metadata rejected",
        self_test=False,
    )
    def test_t2_f03_toml_metadata_bounds(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in (
                "invalid_toml_unknown_field",
                "invalid_toml_reserved_id",
                "invalid_toml_html_metadata",
                "invalid_http_wildcard_origin",
            ):
                _upload_negative_package(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F03-02",
        tier=2,
        feature_id=3,
        name="Non-exact SemVer, Phase 2 worlds, unsupported hooks, and cross-world tables rejected",
        self_test=False,
    )
    def test_t2_f03_semver_world_hook_bounds(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in (
                "invalid_toml_bad_semver",
                "invalid_toml_bad_api",
                "invalid_toml_bad_world",
                "invalid_toml_phase2_world",
                "invalid_toml_unsupported_hook",
                "invalid_toml_cross_world_table",
                "invalid_hook_priority_oob",
            ):
                _upload_negative_package(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F03-03",
        tier=2,
        feature_id=3,
        name="Bounded JSON Schema hazards (remote/recursive $ref, depth>16, cron, secret in array) rejected",
        self_test=False,
    )
    def test_t2_f03_schema_hazards(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in (
                "invalid_schema_remote_ref",
                "invalid_schema_recursive_ref",
                "invalid_schema_bad_widget",
                "invalid_schema_cron_format",
                "invalid_schema_secret_in_array",
                "invalid_schema_additional_props_true",
                "invalid_schema_depth_exceeded",
            ):
                _upload_negative_package(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F03-04",
        tier=2,
        feature_id=3,
        name="PUT /config rejects out-of-range, missing required, unknown prop, and secret in values with fields[]",
        self_test=False,
    )
    def test_t2_f03_config_value_validation_errors(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f03val')}",
            schema_override=build_secret_config_schema(),
            enable=False,
        )
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        err = assert_api_error(
            lambda: ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pkg.plugin_id}/config",
                json_body={
                    "schema_revision": cfg["schema_revision"],
                    "values": {"threshold": 999999, "api_token": "leaked-in-values", "extra": 1},
                    "secrets": {},
                },
                if_match=str(cfg["revision"]),
                idempotency_key=uniq("badcfg"),
            ),
            (400,),
            label="invalid config values",
        )
        assert err.fields, f"Expected field-level errors, got {err!r}"

    @registry.case(
        id="T2-F03-05",
        tier=2,
        feature_id=3,
        name="UI asset manifest hazards (.html, .map, MIME, sha256, self-listed, >1024 files, >8MiB) rejected",
        self_test=False,
    )
    def test_t2_f03_ui_asset_manifest_hazards(ctx: TestContext) -> None:
        set_developer_mode(ctx, True)
        try:
            for kind in (
                "invalid_ui_html",
                "invalid_ui_map",
                "invalid_ui_wrong_sha256",
                "invalid_ui_uppercase_sha256",
                "invalid_ui_mime_mismatch",
                "invalid_ui_self_listed_manifest",
                "invalid_ui_too_many_files",
                "invalid_ui_oversized_file",
            ):
                _upload_negative_package(ctx, build_negative_cmplugin(kind))
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #4: plugin_trust_keys DDL & Trust Key Admin APIs (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F04-01",
        tier=2,
        feature_id=4,
        name="Pinning malformed minisign public key or empty label fails with 400",
        self_test=False,
    )
    def test_t2_f04_malformed_key(ctx: TestContext) -> None:
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-trust-keys/prepare",
                json_body={"public_key": "not-a-minisign-key", "label": ""},
            ),
            (400,),
            label="malformed trust key",
        )

    @registry.case(
        id="T2-F04-02",
        tier=2,
        feature_id=4,
        name="Pinning duplicate key_id or fingerprint fails with 409",
        self_test=False,
    )
    def test_t2_f04_duplicate_key(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("dup1"))
        prep = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-trust-keys/prepare",
            json_body={"public_key": kp.public_key_text, "label": uniq("dup2")},
        )
        reauth = obtain_reauth(ctx, purpose="trust-key:create")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-trust-keys",
                json_body={
                    "public_key": kp.public_key_text,
                    "fingerprint": prep["fingerprint"],
                    "consent_token": prep["consent_token"],
                    "reauth_token": reauth,
                    "label": "dup2",
                },
                if_match="0",
                idempotency_key=uniq("dupkey"),
            ),
            (409,),
            label="duplicate trust key",
        )

    @registry.case(
        id="T2-F04-03",
        tier=2,
        feature_id=4,
        name="Trust key pin/revoke with wrong-purpose reauth or stale consent fails",
        self_test=False,
    )
    def test_t2_f04_reauth_and_consent_enforcement(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        prep = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-trust-keys/prepare",
            json_body={"public_key": kp.public_key_text, "label": uniq("badreauth")},
        )
        wrong_reauth = obtain_reauth(ctx, purpose="plugin:upload")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-trust-keys",
                json_body={
                    "public_key": kp.public_key_text,
                    "fingerprint": prep["fingerprint"],
                    "consent_token": prep["consent_token"],
                    "reauth_token": wrong_reauth,
                    "label": "bad",
                },
                if_match="0",
                idempotency_key=uniq("badre"),
            ),
            (401, 403),
            label="wrong-purpose reauth_token on trust key pin",
        )
        good_reauth = obtain_reauth(ctx, purpose="trust-key:create")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-trust-keys",
                json_body={
                    "public_key": kp.public_key_text,
                    "fingerprint": prep["fingerprint"],
                    "consent_token": "stale-or-forged-consent",
                    "reauth_token": good_reauth,
                    "label": "bad",
                },
                if_match="0",
                idempotency_key=uniq("badcon"),
            ),
            (400, 409),
            label="stale consent_token on trust key pin",
        )

    @registry.case(
        id="T2-F04-04",
        tier=2,
        feature_id=4,
        name="Trust key revoke enforces If-Match (428/409) and 404 on unknown keyId",
        self_test=False,
    )
    def test_t2_f04_revoke_cas_and_404(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned = pin_trust_key(ctx, kp, label=uniq("cas"))
        reauth = obtain_reauth(ctx, purpose="trust-key:revoke")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugin-trust-keys/{pinned['key_id']}/revoke",
                json_body={"reason": "no if-match", "reauth_token": reauth},
                idempotency_key=uniq("noim"),
            ),
            (428,),
            label="missing If-Match on trust key revoke",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugin-trust-keys/{pinned['key_id']}/revoke",
                json_body={"reason": "stale if-match", "reauth_token": reauth},
                if_match="999",
                idempotency_key=uniq("staleim"),
            ),
            (409,),
            label="stale If-Match on trust key revoke",
        )
        r404 = ctx.client.raw_request(
            "POST",
            "/api/v1/plugin-trust-keys/FFFFFFFFFFFFFFFF/revoke",
            body=json.dumps({"reason": "x", "reauth_token": reauth}).encode("utf-8"),
            headers={"Content-Type": "application/json", "If-Match": '"1"', "Idempotency-Key": uniq("k404")},
        )
        assert r404.status == 404

    @registry.case(
        id="T2-F04-05",
        tier=2,
        feature_id=4,
        name="Member user calling /plugin-trust-keys* receives 403 auth.forbidden",
        self_test=False,
    )
    def test_t2_f04_member_forbidden(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-trust-keys")
        _, mc = create_member_client(ctx)
        assert_api_error(
            lambda: mc.get("/api/v1/plugin-trust-keys"),
            (403,),
            label="member GET /plugin-trust-keys",
        )

    # -----------------------------------------------------------------------
    # Feature #5: Staged Install & Crash Recovery (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F05-01",
        tier=2,
        feature_id=5,
        name="Installing same (plugin_id, version) with different package_hash returns 409 plugin.package_conflict",
        self_test=False,
    )
    def test_t2_f05_package_conflict(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('pconf')}"
        pkg1, kp, _ = setup_signed_plugin_ready(ctx, plugin_id=pid, version="1.0.0", enable=False)
        pkg2 = build_cmplugin(
            plugin_id=pid,
            version="1.0.0",
            extra_files={"README.md": b"# different bytes same version\n"},
        )
        sig2 = sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sig2)
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations/prepare",
                json_body={"upload_id": up2["upload_id"]},
                if_match="0",
                idempotency_key=uniq("pconf"),
            ),
            (409,),
            label="same id+version different package_hash",
        )

    @registry.case(
        id="T2-F05-02",
        tier=2,
        feature_id=5,
        name="Concurrent install/op on same plugin_id fails with 409 plugin.operation_active",
        self_test=False,
    )
    def test_t2_f05_operation_active_lock(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("actlock"))
        pid = f"org.example.{uniq('actlock')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, pin_key=False)
        op1 = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/enable",
            json_body={},
            if_match=str(plugin["revision"]),
            idempotency_key=uniq("op1"),
        )
        # Immediately fire a second lifecycle op with a distinct Idempotency-Key before op1 finishes
        try:
            err = assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/enable",
                    json_body={},
                    if_match=str(int(plugin["revision"]) + 1),
                    idempotency_key=uniq("op2"),
                ),
                (409,),
                label="concurrent enable while operation_active",
            )
            assert err.status == 409
        finally:
            poll_operation(ctx, op1["operation_id"])

    @registry.case(
        id="T2-F05-03",
        tier=2,
        feature_id=5,
        name="Reusing Idempotency-Key with different request body fails with 409 plugin.idempotency_conflict",
        self_test=False,
    )
    def test_t2_f05_idempotency_conflict(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("idemc"))
        pkg1 = build_cmplugin(plugin_id=f"org.example.{uniq('ic1')}", kind="hook")
        pkg2 = build_cmplugin(plugin_id=f"org.example.{uniq('ic2')}", kind="hook")
        up1 = upload_cmplugin(ctx, pkg1.bytes, sig_text=sign_cmplugin_sha256(pkg1.bytes, kp))
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp))
        shared_key = uniq("sharedidem")
        op1 = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up1["upload_id"]},
            if_match="0",
            idempotency_key=shared_key,
        )
        poll_operation(ctx, op1["operation_id"])
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations/prepare",
                json_body={"upload_id": up2["upload_id"]},
                if_match="0",
                idempotency_key=shared_key,
            ),
            (409,),
            label="idempotency conflict with different payload",
        )

    @registry.case(
        id="T2-F05-04",
        tier=2,
        feature_id=5,
        name="Missing If-Match returns 428 and wrong If-Match returns 409 on installations",
        self_test=False,
    )
    def test_t2_f05_precondition_and_revision_conflict(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("ifm"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('ifm')}", kind="hook")
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sign_cmplugin_sha256(pkg.bytes, kp))
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations/prepare",
                json_body={"upload_id": up["upload_id"]},
                idempotency_key=uniq("noim"),
            ),
            (428,),
            label="missing If-Match on installations/prepare",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations/prepare",
                json_body={"upload_id": up["upload_id"]},
                if_match="99",
                idempotency_key=uniq("badim"),
            ),
            (409,),
            label="wrong If-Match on installations/prepare",
        )

    @registry.case(
        id="T2-F05-05",
        tier=2,
        feature_id=5,
        name="SIGKILL mid-install never leaves enabled plugin; stale consent_token rejected with 409",
        self_test=False,
    )
    def test_t2_f05_kill_no_auto_enable_and_stale_consent(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("knoen"))
        pid = f"org.example.{uniq('knoen')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sign_cmplugin_sha256(pkg.bytes, kp))
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
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": "wrong-consent-token",
                    "grants": grants,
                },
                if_match="0",
                idempotency_key=uniq("badcon"),
            ),
            (409,),
            label="stale/wrong consent_token on install",
        )
        inst_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations",
            json_body={
                "preparation_id": prep["preparation_id"],
                "consent_token": prep["consent_token"],
                "grants": grants,
            },
            if_match="0",
            idempotency_key=uniq("instk"),
        )
        restart_and_relogin(ctx, kill=True)
        op_after = ctx.call_phase1("GET", f"/api/v1/plugin-operations/{inst_op['operation_id']}")
        assert op_after.get("operation_id") == inst_op["operation_id"]
        plugins = ctx.call_phase1("GET", "/api/v1/plugins")
        assert all(p["lifecycle"] != "enabled" for p in plugins if p["id"] == pid)

    # -----------------------------------------------------------------------
    # Feature #6: Lifecycle State Machine (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F06-01",
        tier=2,
        feature_id=6,
        name="PUT /config, permissions/grant, and state/reset while enabled fail with 409 plugin.not_drained",
        self_test=False,
    )
    def test_t2_f06_not_drained_rejections(ctx: TestContext) -> None:
        pkg, _, enabled = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06nd')}", enable=True
        )
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        assert_api_error(
            lambda: ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pkg.plugin_id}/config",
                json_body={"schema_revision": cfg["schema_revision"], "values": {"dry_run": False}, "secrets": {}},
                if_match=str(cfg["revision"]),
                idempotency_key=uniq("ndcfg"),
            ),
            (409,),
            label="PUT /config while enabled",
        )
        perms = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/permissions")
        granted = (
            perms.get("effective_grants") or perms.get("grants") or []
            if isinstance(perms, dict)
            else (perms if isinstance(perms, list) else [])
        )
        prep_perm = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/permissions/prepare",
            json_body={"requested_grants": granted},
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/permissions/grant",
                json_body={
                    "consent_token": prep_perm["consent_token"],
                    "grants": extract_prep_grants(prep_perm) or granted,
                },
                if_match=str(enabled["revision"]),
                idempotency_key=uniq("ndgrant"),
            ),
            (409,),
            label="permissions/grant while enabled",
        )
        prep_sr = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/state/reset/prepare",
            json_body={"security_scopes": ["global"]},
        )
        reauth_sr = obtain_reauth(ctx, purpose="plugin:state-reset")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/state/reset",
                json_body={
                    "security_scopes": ["global"],
                    "consent_token": prep_sr["consent_token"],
                    "reauth_token": reauth_sr,
                },
                if_match=str(enabled["revision"]),
                idempotency_key=uniq("ndsr"),
            ),
            (409,),
            label="state/reset while enabled",
        )

    @registry.case(
        id="T2-F06-02",
        tier=2,
        feature_id=6,
        name="Enabling plugin with missing required config fails validation and stays disabled",
        self_test=False,
    )
    def test_t2_f06_enable_invalid_config_rejected(ctx: TestContext) -> None:
        req_schema = {
            "type": "object",
            "additionalProperties": False,
            "required": ["must_set"],
            "properties": {"must_set": {"type": "string", "minLength": 5}},
        }
        pkg, _, plugin = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f06badcfg')}",
            schema_override=req_schema,
            enable=False,
        )
        try:
            op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/enable",
                json_body={},
                if_match=str(plugin["revision"]),
                idempotency_key=uniq("enbad"),
            )
        except ApiError as exc:
            assert exc.status in (400, 409, 422), (
                f"Expected 400/409/422 for invalid config enable, got {exc}"
            )
        else:
            done = poll_operation(ctx, op["operation_id"])
            assert done["state"] in ("failed", "rejected"), f"Expected failed/rejected, got {done!r}"
        after = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}")
        assert after["lifecycle"] in ("installed-disabled", "disabled")

    @registry.case(
        id="T2-F06-03",
        tier=2,
        feature_id=6,
        name="Wildcard capability (* or hooks:*) in permissions/grant rejected with 400",
        self_test=False,
    )
    def test_t2_f06_wildcard_grant_rejected(ctx: TestContext) -> None:
        pkg, _, plugin = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06wc')}", enable=False
        )
        perms = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/permissions")
        sample = (
            (perms.get("requested_grants") or perms.get("effective_grants") or perms.get("grants") or [])
            if isinstance(perms, dict)
            else (perms if isinstance(perms, list) else [])
        )
        if sample and isinstance(sample[0], dict):
            req_wild = [
                {"capability": "hooks:*", "scope": "Global", "sensitive": False},
                {"capability": "*", "scope": "Global", "sensitive": False},
            ]
        else:
            req_wild = ["hooks:*", "*"]
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/permissions/prepare",
                json_body={"requested_grants": req_wild},
            ),
            (400,),
            label="wildcard permission grant",
        )
        assert plugin["id"] == pkg.plugin_id

    @registry.case(
        id="T2-F06-04",
        tier=2,
        feature_id=6,
        name="Lifecycle endpoints enforce If-Match (428/409) and 404 on unknown plugin",
        self_test=False,
    )
    def test_t2_f06_cas_and_404(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06cas')}", enable=False
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/enable",
                json_body={},
                idempotency_key=uniq("noim"),
            ),
            (428,),
            label="enable missing If-Match",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/enable",
                json_body={},
                if_match="999",
                idempotency_key=uniq("badim"),
            ),
            (409,),
            label="enable stale If-Match",
        )
        r404 = ctx.client.raw_request(
            "POST",
            "/api/v1/plugins/org.example.does-not-exist/enable",
            body=b"{}",
            headers={"Content-Type": "application/json", "If-Match": '"1"', "Idempotency-Key": uniq("p404")},
        )
        assert r404.status == 404

    @registry.case(
        id="T2-F06-05",
        tier=2,
        feature_id=6,
        name="Invalid lifecycle transitions (enable when enabled, disable when disabled) return 409",
        self_test=False,
    )
    def test_t2_f06_invalid_transitions(ctx: TestContext) -> None:
        pkg, _, enabled = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06tr')}", enable=True
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/enable",
                json_body={},
                if_match=str(enabled["revision"]),
                idempotency_key=uniq("en2"),
            ),
            (409,),
            label="enable already-enabled plugin",
        )
        _, _, disabled = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f06trd')}", enable=False
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{disabled['id']}/disable",
                json_body={"reason": "already disabled"},
                if_match=str(disabled["revision"]),
                idempotency_key=uniq("dis2"),
            ),
            (409,),
            label="disable already-disabled plugin",
        )

    # -----------------------------------------------------------------------
    # Feature #7: Upgrade & Rollback (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F07-01",
        tier=2,
        feature_id=7,
        name="Upgrade across state_schema 1->2 with state_policy=preserve rejected with 409",
        self_test=False,
    )
    def test_t2_f07_schema_mismatch_preserve(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('f07sm')}"
        _, kp, p1 = setup_signed_plugin_ready(ctx, plugin_id=pid, version="1.0.0", state_schema=1, enable=False)
        pkg2 = build_cmplugin(plugin_id=pid, version="2.0.0", state_schema=2)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp))
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={"upload_id": up2["upload_id"], "state_policy": "preserve", "target_config": {}},
                if_match=str(p1["revision"]),
                idempotency_key=uniq("smpres"),
            ),
            (409,),
            label="preserve across state_schema change",
        )

    @registry.case(
        id="T2-F07-02",
        tier=2,
        feature_id=7,
        name="Upgrade with state_policy=reset missing reauth_token is rejected",
        self_test=False,
    )
    def test_t2_f07_reset_missing_reauth(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('f07nr')}"
        _, kp, p1 = setup_signed_plugin_ready(ctx, plugin_id=pid, version="1.0.0", state_schema=1, enable=False)
        pkg2 = build_cmplugin(plugin_id=pid, version="2.0.0", state_schema=2)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp))
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={
                "upload_id": up2["upload_id"],
                "state_policy": "reset",
                "target_config": {
                    "schema_revision": cfg["schema_revision"],
                    "values": {"dry_run": True},
                    "secrets": {},
                },
            },
            if_match=str(p1["revision"]),
            idempotency_key=uniq("upprep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        grants = extract_prep_grants(prep)
        assert len(grants) >= 1, f"Expected non-empty grants in prep: {prep!r}"
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": grants,
                    "state_policy": "reset",
                    "target_config_digest": prep.get("target_config_digest", ""),
                },
                if_match=str(p1["revision"]),
                idempotency_key=uniq("noreauth"),
            ),
            (400, 401, 403),
            label="upgrade reset without reauth_token",
        )

    @registry.case(
        id="T2-F07-03",
        tier=2,
        feature_id=7,
        name="Consent token mismatch on target_config_digest or state_policy returns 409 plugin.consent_stale",
        self_test=False,
    )
    def test_t2_f07_consent_digest_mismatch(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('f07cd')}"
        _, kp, p1 = setup_signed_plugin_ready(ctx, plugin_id=pid, version="1.0.0", state_schema=1, enable=False)
        pkg2 = build_cmplugin(plugin_id=pid, version="1.1.0", state_schema=1)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp))
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"upload_id": up2["upload_id"], "state_policy": "preserve", "target_config": {}},
            if_match=str(p1["revision"]),
            idempotency_key=uniq("cdprep"),
        )
        prep = (poll_operation(ctx, prep_op["operation_id"]).get("result")) or {}
        grants = extract_prep_grants(prep)
        assert len(grants) >= 1, f"Expected non-empty grants in prep: {prep!r}"
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": grants,
                    "state_policy": "preserve",
                    "target_config_digest": "f" * 64,
                },
                if_match=str(p1["revision"]),
                idempotency_key=uniq("cdexec"),
            ),
            (409,),
            label="mismatched target_config_digest",
        )

    @registry.case(
        id="T2-F07-04",
        tier=2,
        feature_id=7,
        name="Rollback to version whose signing key was revoked is rejected",
        self_test=False,
    )
    def test_t2_f07_rollback_revoked_key(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('f07rb')}"
        kp1 = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        kp2 = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned1 = pin_trust_key(ctx, kp1, label=uniq("rbk1"))
        pin_trust_key(ctx, kp2, label=uniq("rbk2"))
        pkg1 = build_cmplugin(plugin_id=pid, version="1.0.0", state_schema=1)
        p1, _ = prepare_and_install_plugin(ctx, pkg1, kp=kp1, pin_key=False)
        pkg2 = build_cmplugin(plugin_id=pid, version="1.1.0", state_schema=1)
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp2))
        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/upgrades/prepare",
            json_body={"upload_id": up2["upload_id"], "state_policy": "preserve", "target_config": {}},
            if_match=str(p1["revision"]),
            idempotency_key=uniq("rbup"),
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
            idempotency_key=uniq("rbup2"),
        )
        assert poll_operation(ctx, up_op["operation_id"])["state"] == "succeeded"
        reauth_rev = obtain_reauth(ctx, purpose="trust-key:revoke")
        rev_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugin-trust-keys/{pinned1['key_id']}/revoke",
            json_body={"reason": "revoke v1", "reauth_token": reauth_rev},
            if_match=str(pinned1["revision"]),
            idempotency_key=uniq("rbrev"),
        )
        rev_done = poll_operation(ctx, rev_op["operation_id"])
        assert rev_done["state"] == "succeeded"
        p2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={"version": "1.0.0", "state_policy": "preserve", "target_config": {}},
                if_match=str(p2["revision"]),
                idempotency_key=uniq("rbprepbad"),
            ),
            (403, 409),
            label="rollback to version signed by revoked key",
        )

    @registry.case(
        id="T2-F07-05",
        tier=2,
        feature_id=7,
        name="state/reset while enabled or without reauth_token is rejected",
        self_test=False,
    )
    def test_t2_f07_state_reset_guards(ctx: TestContext) -> None:
        pkg, _, enabled = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f07sr')}", enable=True
        )
        prep = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/state/reset/prepare",
            json_body={"security_scopes": ["global"]},
        )
        reauth = obtain_reauth(ctx, purpose="plugin:state-reset")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/state/reset",
                json_body={
                    "security_scopes": ["global"],
                    "consent_token": prep["consent_token"],
                    "reauth_token": reauth,
                },
                if_match=str(enabled["revision"]),
                idempotency_key=uniq("sren"),
            ),
            (409,),
            label="state/reset while enabled",
        )
        disabled = disable_plugin(ctx, pkg.plugin_id)
        prep2 = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/state/reset/prepare",
            json_body={"security_scopes": ["global"]},
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pkg.plugin_id}/state/reset",
                json_body={
                    "security_scopes": ["kv"],
                    "consent_token": prep2["consent_token"],
                },
                if_match=str(disabled["revision"]),
                idempotency_key=uniq("srnor"),
            ),
            (400, 401, 403),
            label="state/reset without reauth_token",
        )

    # -----------------------------------------------------------------------
    # Feature #8: Core Admin Plugin APIs & /plugin-reauth (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F08-01",
        tier=2,
        feature_id=8,
        name="Unauthenticated gets 401 and member gets 403 on Admin plugin routes",
        self_test=False,
    )
    def test_t2_f08_auth_and_rbac(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        anon = ctx.new_client()
        assert_api_error(lambda: anon.get("/api/v1/plugins"), (401,), label="anon GET /plugins")
        _, mc = create_member_client(ctx)
        for route in ("/api/v1/plugins", "/api/v1/settings/plugins", "/api/v1/plugin-hook-catalog"):
            assert_api_error(lambda r=route: mc.get(r), (403,), label=f"member GET {route}")

    @registry.case(
        id="T2-F08-02",
        tier=2,
        feature_id=8,
        name="Bad password on /plugin-reauth returns 401 and wrong-purpose token is rejected",
        self_test=False,
    )
    def test_t2_f08_reauth_failures(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-reauth",
                json_body={"password": "wrong-admin-password", "purpose": "plugin:upload"},
            ),
            (401,),
            label="bad password on /plugin-reauth",
        )
        wrong_tok = obtain_reauth(ctx, purpose="developer-mode:enable")
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('f08wp')}", kind="hook")
        set_developer_mode(ctx, True)
        try:
            assert_api_error(
                lambda: post_multipart_phase1(
                    ctx.client,
                    "/api/v1/plugin-uploads",
                    fields={"upload_id": str(uuid.uuid4())},
                    files=[("file", "plugin.cmplugin", pkg.bytes, "application/zip")],
                    reauth_token=wrong_tok,
                    idempotency_key=uniq("wp"),
                    if_match="0",
                ),
                (401, 403),
                label="wrong-purpose reauth_token on /plugin-uploads",
            )
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T2-F08-03",
        tier=2,
        feature_id=8,
        name="Invalid or missing cursor / unknown operation ID returns clean 400/404",
        self_test=False,
    )
    def test_t2_f08_cursor_and_op_404(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        assert_api_error(
            lambda: ctx.call_phase1("GET", "/api/v1/plugins?cursor=not-a-valid-cursor"),
            (400,),
            label="invalid cursor on GET /plugins",
        )
        r = ctx.client.raw_request("GET", "/api/v1/plugin-operations/00000000-0000-0000-0000-000000000000")
        assert r.status == 404

    @registry.case(
        id="T2-F08-04",
        tier=2,
        feature_id=8,
        name="PUT /config rejects empty secret replace and unknown secret key with 400",
        self_test=False,
    )
    def test_t2_f08_secret_validation_boundaries(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f08sec')}",
            schema_override=build_secret_config_schema(),
            enable=False,
        )
        cfg = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        assert_api_error(
            lambda: ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pkg.plugin_id}/config",
                json_body={
                    "schema_revision": cfg["schema_revision"],
                    "values": {"profile_label": "x"},
                    "secrets": {"api_token": {"action": "replace", "value": ""}},
                },
                if_match=str(cfg["revision"]),
                idempotency_key=uniq("empsec"),
            ),
            (400,),
            label="empty secret replace",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pkg.plugin_id}/config",
                json_body={
                    "schema_revision": cfg["schema_revision"],
                    "values": {"profile_label": "x"},
                    "secrets": {"nonexistent_secret": {"action": "replace", "value": "abc"}},
                },
                if_match=str(cfg["revision"]),
                idempotency_key=uniq("unksec"),
            ),
            (400,),
            label="unknown secret key",
        )

    @registry.case(
        id="T2-F08-05",
        tier=2,
        feature_id=8,
        name="Phase 2 deferred routes all return 404 while Phase 1 plugin router is active",
        self_test=False,
    )
    def test_t2_f08_phase2_routes_absent(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        for path in (
            "/api/v1/plugin-repos",
            "/api/v1/list-subscriptions",
            "/api/v1/plugins/org.example.none/endpoint-keys",
            "/api/v1/plugins/org.example.none/state/exports",
            "/api/v1/plugins/org.example.none/widgets",
        ):
            r = ctx.client.raw_request("GET", path)
            assert r.status == 404, f"Phase 2 route {path} must return 404, got {r.status}"
