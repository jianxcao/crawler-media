"""Tier 3: Pairwise Cross-Feature Combination Tests.

Retains T3-INFRA-01 (self_test=True smoke check) and implements 29 pairwise
cross-feature combination tests (T3-P01 through T3-P29, self_test=False)
covering every feature #1-#26 from PROJECT.md in >= 2 cross-feature pairs.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import time
from typing import Any, Dict, List, Optional
import uuid
import zipfile

from ..lib.client import ApiClient, ApiError
from ..lib.fixtures import (
    build_cmplugin,
    build_dishonest_torrent,
    build_negative_cmplugin,
    build_secret_config_schema,
    build_torrent_bytes,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
    verify_minisign,
)
from ..lib.instance import InstanceSupervisor
from . import SuiteRegistry, TestContext
from ._helpers import (
    assert_api_error,
    create_member_client,
    create_subscribe,
    disable_plugin,
    enable_plugin,
    ensure_downloader,
    ensure_library,
    ensure_plugin_enabled,
    ensure_site,
    extract_prep_grants,
    grant_site_browser_permission,
    grant_ui_trust,
    make_temp_web_dir,
    obtain_reauth,
    pin_trust_key,
    poll_operation,
    post_multipart_phase1,
    prepare_and_install_plugin,
    put_browser_settings,
    resolve_bin_path,
    restart_and_relogin,
    run_plugin_cli,
    set_developer_mode,
    setup_signed_plugin_ready,
    trigger_job,
    uniq,
    unwrap_items,
    unwrap_list,
    upload_cmplugin,
)
from .t1_p5 import _set_watch_intake, _wait_for_webhooks, poll_operation_removal
from .t1_p6 import _find_first_party_plugin


def _require_dict_key(obj: Any, key: str, label: str) -> Any:
    assert isinstance(obj, dict) and key in obj, (
        f"Expected dict with {key!r} for {label}, got {obj!r}"
    )
    return obj[key]


def _list_all_paginated(ctx: TestContext, path: str, label: str) -> List[Any]:
    items: List[Any] = []
    cursor: Optional[str] = None
    seen_cursors = set()
    sep = "&" if "?" in path else "?"
    while True:
        cur_path = f"{path}{sep}limit=100" + (f"&cursor={cursor}" if cursor else "")
        try:
            _, env = ctx.client.request_envelope("GET", cur_path)
        except ApiError as exc:
            if exc.status in (404, 405, 501):
                from . import UnimplementedFeatureError

                raise UnimplementedFeatureError(f"GET {cur_path}", f"HTTP {exc.status}") from exc
            raise
        data = env.get("data")
        items.extend(unwrap_list(data, label))
        nxt = env.get("next_cursor")
        if not nxt or nxt in seen_cursors:
            break
        seen_cursors.add(nxt)
        cursor = str(nxt)
    return items


def _extract_target_config_digest(prep: Any, label: str = "upgrades/prepare") -> str:
    assert isinstance(prep, dict), f"Expected dict from {label}, got {prep!r}"
    cfg_diff = prep.get("config_diff")
    digest = None
    if isinstance(cfg_diff, dict):
        digest = cfg_diff.get("target_config_digest")
    if not digest:
        digest = prep.get("target_config_digest")
    assert isinstance(digest, str) and len(digest) > 0, (
        f"Missing target_config_digest in {label}: {prep!r}"
    )
    return digest


def _poll_op_best_effort(ctx: TestContext, op_id: str, timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        r = ctx.client.raw_request("GET", f"/api/v1/plugin-operations/{op_id}")
        if r.status != 200 or not r.body.strip().startswith(b"{"):
            return
        parsed = r.json()
        data = parsed.get("data") if isinstance(parsed, dict) else None
        if isinstance(data, dict) and data.get("state") in (
            "succeeded",
            "failed",
            "cancelled",
            "rejected",
        ):
            return
        time.sleep(0.1)


def _safe_cleanup_plugin(ctx: TestContext, plugin_id: str) -> None:
    r = ctx.client.raw_request("GET", f"/api/v1/plugins/{plugin_id}")
    if r.status != 200 or not r.body.strip().startswith(b"{"):
        return
    body = r.json()
    pdata = body.get("data") if isinstance(body, dict) and isinstance(body.get("data"), dict) else None
    if pdata is None:
        return
    rev = str(pdata.get("revision", 1))
    if pdata.get("lifecycle") == "enabled":
        d_resp = ctx.client.raw_request(
            "POST",
            f"/api/v1/plugins/{plugin_id}/disable",
            body=json.dumps({"reason": "e2e-cleanup"}).encode("utf-8"),
            headers={
                "Content-Type": "application/json",
                "If-Match": f'"{rev}"',
                "Idempotency-Key": uniq("clndis"),
            },
        )
        if d_resp.status in (200, 202) and d_resp.body.strip().startswith(b"{"):
            dj = d_resp.json()
            op_id = (dj.get("data") or {}).get("operation_id") if isinstance(dj, dict) else None
            if op_id:
                _poll_op_best_effort(ctx, str(op_id))
            r2 = ctx.client.raw_request("GET", f"/api/v1/plugins/{plugin_id}")
            if r2.status == 200 and r2.body.strip().startswith(b"{"):
                b2 = r2.json()
                pd2 = b2.get("data") if isinstance(b2, dict) and isinstance(b2.get("data"), dict) else None
                if pd2:
                    rev = str(pd2.get("revision", rev))
    u_resp = ctx.client.raw_request(
        "POST",
        f"/api/v1/plugins/{plugin_id}/uninstall",
        body=json.dumps({"remove_private_data": True}).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "If-Match": f'"{rev}"',
            "Idempotency-Key": uniq("clnun"),
        },
    )
    if u_resp.status in (200, 202) and u_resp.body.strip().startswith(b"{"):
        uj = u_resp.json()
        op_id = (uj.get("data") or {}).get("operation_id") if isinstance(uj, dict) else None
        if op_id:
            _poll_op_best_effort(ctx, str(op_id))


def _safe_restore_developer_mode(ctx: TestContext, target_enabled: bool = False) -> None:
    r = ctx.client.raw_request("GET", "/api/v1/settings/plugins")
    if r.status != 200 or not r.body.strip().startswith(b"{"):
        return
    parsed = r.json()
    data = parsed.get("data") if isinstance(parsed, dict) and isinstance(parsed.get("data"), dict) else None
    if data is None:
        return
    current = data["developer_mode"] if "developer_mode" in data else data.get("enabled")
    if bool(current) == target_enabled:
        return
    rev = str(data.get("revision", 0))
    if not target_enabled:
        d_resp = ctx.client.raw_request(
            "POST",
            "/api/v1/settings/plugins/developer/disable",
            body=b"{}",
            headers={
                "Content-Type": "application/json",
                "If-Match": f'"{rev}"',
                "Idempotency-Key": uniq("clndev"),
            },
        )
        if d_resp.status in (200, 202) and d_resp.body.strip().startswith(b"{"):
            dj = d_resp.json()
            op_id = (dj.get("data") or {}).get("operation_id") if isinstance(dj, dict) else None
            if op_id:
                _poll_op_best_effort(ctx, str(op_id))


def _safe_restore_browser(ctx: TestContext) -> None:
    r = ctx.client.raw_request("GET", "/api/v1/settings/browser")
    if r.status != 200:
        return
    ctx.client.raw_request(
        "PUT",
        "/api/v1/settings/browser",
        body=json.dumps({"cdp": {"enabled": False}}).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )


def _safe_restore_watch_intake(ctx: TestContext, prev_watch: Optional[str]) -> None:
    r = ctx.client.raw_request("GET", "/api/v1/settings/transfer")
    if r.status != 200 or not r.body.strip().startswith(b"{"):
        return
    parsed = r.json()
    data = parsed.get("data") if isinstance(parsed, dict) and isinstance(parsed.get("data"), dict) else None
    if data is None or "revision" not in data:
        return
    ctx.client.raw_request(
        "PATCH",
        "/api/v1/settings/transfer",
        body=json.dumps(
            {
                "expected_revision": data["revision"],
                "watch_intake": str(prev_watch) if prev_watch is not None else None,
            }
        ).encode("utf-8"),
        headers={"Content-Type": "application/json"},
    )


def _rewrite_toml_fields(toml_path: Path, updates: Dict[str, str]) -> None:
    lines = toml_path.read_text(encoding="utf-8").splitlines()
    out: List[str] = []
    seen: set = set()
    in_section = False
    first_section_idx: Optional[int] = None
    for line in lines:
        stripped = line.lstrip()
        if not in_section and stripped.startswith("["):
            in_section = True
            first_section_idx = len(out)
        if not in_section and stripped and not stripped.startswith("#") and "=" in stripped:
            lhs = stripped.split("=", 1)[0].strip()
            if lhs in updates:
                out.append(f'{lhs} = "{updates[lhs]}"')
                seen.add(lhs)
                continue
        out.append(line)
    missing = [f'{k} = "{v}"' for k, v in updates.items() if k not in seen]
    if missing:
        idx = len(out) if first_section_idx is None else first_section_idx
        out[idx:idx] = missing
    toml_path.write_text("\n".join(out) + "\n", encoding="utf-8")


def register_cases(registry: SuiteRegistry) -> None:
    @registry.case(
        id="T3-INFRA-01",
        tier=3,
        feature_id=11,
        name="Pairwise mock harness: MockPtSiteServer + MockQbitServer + MockObscuraCdpServer + UI contract",
        self_test=True,
        related_features=(11, 12, 17, 24),
    )
    def test_t3_mock_servers_and_ui_contract(ctx: TestContext) -> None:
        contract = ctx.call_phase1("GET", "/api/v1/plugin-ui-contract")
        assert contract.get("contract") == "1.0.0"
        assert contract.get("react") == "19.3.0"

        pt = ctx.new_pt_site(expected_cookie="uid=42", expected_api_key="mteam-key")
        qb = ctx.new_qbit()
        cdp = ctx.new_cdp()

        # Verify PT site search & torrent download
        pt_client = ApiClient(pt.url)
        search_resp = pt_client.raw_request(
            "GET",
            "/torrents.php?search=E2E",
            headers={"Cookie": "uid=42"},
        )
        assert search_resp.status == 200
        assert "logout.php" in search_resp.text()
        assert "E2E.Demo.Release" in search_resp.text()

        # Verify qBittorrent login + multipart .torrent add
        qb_client = ApiClient(qb.url)
        login_resp = qb_client.raw_request(
            "POST",
            "/api/v2/auth/login",
            body=b"username=admin&password=adminadmin",
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
        assert login_resp.status == 200
        assert "SID=" in (login_resp.header("set-cookie") or "")

        tf = build_torrent_bytes(name="T3.Pairwise.Item", length=2048)
        sid_cookie = login_resp.header("set-cookie", "").split(";", 1)[0]
        add_resp = qb_client.post_multipart(
            "/api/v2/torrents/add",
            fields={"tags": "crawler-media", "paused": "false"},
            files=[("torrents", "item.torrent", tf.bytes, "application/x-bittorrent")],
            headers={"Cookie": sid_cookie},
            expect_envelope=False,
        )
        assert add_resp.status == 200
        assert tf.info_hash in qb.torrents

        # Verify CDP /json/version matches server host/port
        cdp_client = ApiClient(cdp.url)
        ver_resp = cdp_client.raw_request("GET", "/json/version")
        assert ver_resp.status == 200
        ver_json = json.loads(ver_resp.body.decode("utf-8"))
        assert ver_json["webSocketDebuggerUrl"].startswith(f"ws://127.0.0.1:{cdp.port}/")

        # Verify multi-file plugin package assembly
        pkg = build_cmplugin(kind="task", include_ui=True, include_site_yaml=True)
        assert "components/tasks.wasm" in pkg.entries
        assert "ui/asset-manifest.json" in pkg.entries
        assert "sites/e2e-demo.yaml" in pkg.entries

    # -----------------------------------------------------------------------
    # P1c Pairwise Combinations (T3-P01 .. T3-P08)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T3-P01",
        tier=3,
        feature_id=1,
        name="Archive compression (Deflate+Store) + Minisign/DevMode policy + Schema/UI manifest validation",
        self_test=False,
        related_features=(1, 2, 3),
    )
    def test_t3_p01_archive_minisign_devmode_manifest(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("t3p01"))
        pid = f"org.example.{uniq('t3p01')}"
        pkg = build_cmplugin(
            plugin_id=pid,
            kind="hook",
            include_ui=True,
            compress_overrides={"LICENSE": zipfile.ZIP_STORED},
        )
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert up.get("hash") == pkg.sha256_hex, f"Upload hash mismatch: {up!r}"
        assert up.get("signature_status") in ("signed", "trusted", "verified"), (
            f"Expected verified signature_status, got {up!r}"
        )
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up["upload_id"]},
            if_match="0",
            idempotency_key=uniq("t3p01prep"),
        )
        prep_done = poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "installations/prepare"))
        assert prep_done.get("state") == "succeeded", f"Expected succeeded prepare, got {prep_done!r}"
        prep = prep_done.get("result") or prep_done
        assert prep.get("package_hash") == pkg.sha256_hex, f"Expected package_hash match, got {prep!r}"

        # Unsigned valid package must be rejected while developer_mode is disabled
        _safe_restore_developer_mode(ctx, False)
        unsigned_pkg = build_cmplugin(plugin_id=f"org.example.{uniq('t3p01u')}", kind="hook")
        assert_api_error(
            lambda: upload_cmplugin(ctx, unsigned_pkg.bytes, sig_text=None),
            (401, 403),
            label="unsigned upload when developer_mode=False",
        )

        # With developer_mode=True, valid unsigned succeeds, while archive/schema/UI hazards still fail
        try:
            set_developer_mode(ctx, True)
            up_dev = upload_cmplugin(ctx, unsigned_pkg.bytes, sig_text=None)
            assert up_dev.get("hash") == unsigned_pkg.sha256_hex, f"Dev upload mismatch: {up_dev!r}"
            assert up_dev.get("signature_status") in ("unsigned", "development"), (
                f"Expected unsigned/development status, got {up_dev!r}"
            )
            reauth = obtain_reauth(ctx, purpose="plugin:upload")
            for hazard in ("zip_bomb", "invalid_schema_recursive_ref", "invalid_ui_wrong_sha256"):
                bad_bytes = build_negative_cmplugin(hazard)
                assert_api_error(
                    lambda b=bad_bytes: post_multipart_phase1(
                        ctx.client,
                        "/api/v1/plugin-uploads",
                        fields={"upload_id": uniq("bad")},
                        files=[("file", "bad.cmplugin", b, "application/zip")],
                        reauth_token=reauth,
                        idempotency_key=uniq("t3p01bad"),
                        if_match="0",
                    ),
                    (400, 422),
                    label=f"hazard {hazard} under dev mode",
                )
        finally:
            _safe_restore_developer_mode(ctx, False)

    @registry.case(
        id="T3-P02",
        tier=3,
        feature_id=2,
        name="Minisign trust key revocation drains enabled plugin and blocks re-enable and new uploads",
        self_test=False,
        related_features=(2, 4, 6),
    )
    def test_t3_p02_revoke_key_drains_and_blocks(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned = pin_trust_key(ctx, kp, label=uniq("t3p02"))
        key_id = _require_dict_key(pinned, "key_id", "pin_trust_key")
        pid = f"org.example.{uniq('t3p02')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        try:
            p1, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, pin_key=False)
            enabled = enable_plugin(ctx, pid, revision=int(_require_dict_key(p1, "revision", "install")))
            prev_gen = int(_require_dict_key(enabled, "generation", "enable"))

            reauth_rev = obtain_reauth(ctx, purpose="trust-key:revoke")
            rev_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugin-trust-keys/{key_id}/revoke",
                json_body={"reason": "pairwise-revoke", "reauth_token": reauth_rev},
                if_match=str(_require_dict_key(pinned, "revision", "pinned key")),
                idempotency_key=uniq("t3p02rev"),
            )
            assert poll_operation(ctx, _require_dict_key(rev_op, "operation_id", "revoke key"))["state"] == "succeeded"

            keys = _list_all_paginated(ctx, "/api/v1/plugin-trust-keys", "trust keys")
            matched_keys = [k for k in keys if isinstance(k, dict) and k.get("key_id") == key_id]
            assert len(matched_keys) == 1 and matched_keys[0].get("status") == "revoked", (
                f"Expected revoked key {key_id}, got {matched_keys!r}"
            )

            drained = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
            assert drained.get("lifecycle") == "disabled", f"Expected drained disabled, got {drained!r}"
            assert int(drained.get("generation", 0)) > prev_gen, (
                f"Expected generation bump after drain ({prev_gen} -> {drained!r})"
            )

            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/enable",
                    json_body={},
                    if_match=str(drained["revision"]),
                    idempotency_key=uniq("t3p02reen"),
                ),
                (403, 409),
                label="re-enable plugin signed by revoked key",
            )

            pkg2 = build_cmplugin(plugin_id=f"org.example.{uniq('t3p02b')}", kind="hook")
            sig2 = sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True)
            assert_api_error(
                lambda: upload_cmplugin(ctx, pkg2.bytes, sig_text=sig2),
                (401, 403),
                label="upload signed by revoked key",
            )
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P03",
        tier=3,
        feature_id=3,
        name="Secret config schema (profile_label) + /config/validate & redaction + lifecycle edit lock",
        self_test=False,
        related_features=(3, 8, 6),
    )
    def test_t3_p03_secret_config_validate_and_lifecycle_lock(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('t3p03')}"
        try:
            _, _, p0 = setup_signed_plugin_ready(
                ctx,
                plugin_id=pid,
                kind="hook",
                schema_override=build_secret_config_schema(),
                enable=False,
            )
            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            rev0 = int(_require_dict_key(cfg0, "revision", "GET /config"))
            cfg_rev0 = int(cfg0.get("config_revision", rev0))
            schema_rev = str(_require_dict_key(cfg0, "schema_revision", "GET /config"))

            val_res = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/config/validate",
                json_body={
                    "schema_revision": schema_rev,
                    "values": {"profile_label": "prod-1", "dry_run": False, "threshold": 42},
                },
            )
            assert isinstance(val_res, dict) and val_res.get("valid") is True, (
                f"Expected valid config validation, got {val_res!r}"
            )
            cfg_after_val = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            assert int(cfg_after_val.get("config_revision", cfg_after_val["revision"])) == cfg_rev0, (
                f"/config/validate must not mutate config_revision: {cfg_after_val!r}"
            )

            secret_val = "p3-secret-token-77"
            put_res = ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pid}/config",
                json_body={
                    "schema_revision": schema_rev,
                    "values": {"profile_label": "prod-1", "dry_run": False, "threshold": 42},
                    "secrets": {"api_token": {"action": "replace", "value": secret_val}},
                },
                if_match=str(rev0),
                idempotency_key=uniq("t3p03put"),
            )
            assert int(put_res.get("config_revision", put_res.get("revision", 0))) > cfg_rev0, (
                f"Expected config_revision increment, got {put_res!r}"
            )
            put_secs = _require_dict_key(put_res, "secrets", "PUT /config")
            put_tok = _require_dict_key(put_secs, "api_token", "PUT /config secrets")
            assert isinstance(put_tok, dict) and put_tok.get("configured") is True, (
                f"Expected configured api_token in {put_res!r}"
            )
            assert secret_val not in str(put_res), "Secret leaked in PUT /config response"

            cfg1 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            cfg1_secs = _require_dict_key(cfg1, "secrets", "GET /config")
            cfg1_tok = _require_dict_key(cfg1_secs, "api_token", "GET /config secrets")
            assert isinstance(cfg1_tok, dict) and cfg1_tok.get("configured") is True, (
                f"Expected configured api_token in {cfg1!r}"
            )
            assert secret_val not in str(cfg1), "Secret leaked in GET /config response"
            audit = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/audit?limit=50"), "audit")
            assert secret_val not in str(audit), "Secret leaked in plugin audit"

            enable_plugin(ctx, pid)
            assert_api_error(
                lambda: ctx.call_phase1(
                    "PUT",
                    f"/api/v1/plugins/{pid}/config",
                    json_body={
                        "schema_revision": schema_rev,
                        "values": {"profile_label": "prod-2", "dry_run": True, "threshold": 10},
                        "secrets": {},
                    },
                    if_match=str(cfg1["revision"]),
                    idempotency_key=uniq("t3p03lock"),
                ),
                (409,),
                label="PUT /config while enabled",
            )

            disable_plugin(ctx, pid)
            cfg2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            kept = ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pid}/config",
                json_body={
                    "schema_revision": schema_rev,
                    "values": {"profile_label": "prod-2", "dry_run": True, "threshold": 10},
                    "secrets": {"api_token": {"action": "keep"}},
                },
                if_match=str(cfg2["revision"]),
                idempotency_key=uniq("t3p03keep"),
            )
            kept_secs = _require_dict_key(kept, "secrets", "PUT /config keep")
            kept_tok = _require_dict_key(kept_secs, "api_token", "PUT /config keep secrets")
            assert isinstance(kept_tok, dict) and kept_tok.get("configured") is True
            assert secret_val not in str(kept)
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P04",
        tier=3,
        feature_id=4,
        name="Publisher key rotation across upgrade keeps active plugin enabled and blocks stale-key rollback",
        self_test=False,
        related_features=(4, 2, 7),
    )
    def test_t3_p04_key_rotation_and_stale_rollback_block(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('t3p04')}"
        kp1 = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        kp2 = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pinned1 = pin_trust_key(ctx, kp1, label=uniq("t3p04k1"))
        pin_trust_key(ctx, kp2, label=uniq("t3p04k2"))
        pkg1 = build_cmplugin(plugin_id=pid, version="1.0.0", state_schema=1, kind="hook")
        try:
            p1, _ = prepare_and_install_plugin(ctx, pkg1, kp=kp1, pin_key=False)
            enable_plugin(ctx, pid, revision=int(p1["revision"]))
            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            schema_rev = str(cfg0["schema_revision"])

            pkg2 = build_cmplugin(plugin_id=pid, version="1.1.0", state_schema=1, kind="hook")
            up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp2, hashed=True))
            p_dis = disable_plugin(ctx, pid)
            prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "1.1.0",
                    "state_policy": "preserve",
                    "target_config": {"schema_revision": schema_rev, "values": {}, "secrets": {}},
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t3p04up"),
            )
            prep = (poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "upgrades/prepare")).get("result")) or {}
            digest = _extract_target_config_digest(prep)
            up_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": _require_dict_key(prep, "preparation_id", "prep"),
                    "consent_token": _require_dict_key(prep, "consent_token", "prep"),
                    "grants": extract_prep_grants(prep),
                    "state_policy": "preserve",
                    "target_config_digest": digest,
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t3p04exec"),
            )
            assert poll_operation(ctx, _require_dict_key(up_op, "operation_id", "upgrade"))["state"] == "succeeded"
            p2 = enable_plugin(ctx, pid)
            assert p2.get("version") == "1.1.0", f"Expected version 1.1.0, got {p2!r}"

            keys = _list_all_paginated(ctx, "/api/v1/plugin-trust-keys", "trust keys")
            assert any(int(k.get("associated_package_count", 0)) >= 1 for k in keys if isinstance(k, dict)), (
                f"Expected associated_package_count >= 1 in {keys!r}"
            )

            reauth_rev = obtain_reauth(ctx, purpose="trust-key:revoke")
            rev_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugin-trust-keys/{pinned1['key_id']}/revoke",
                json_body={"reason": "rotate-out", "reauth_token": reauth_rev},
                if_match=str(pinned1["revision"]),
                idempotency_key=uniq("t3p04rev"),
            )
            assert poll_operation(ctx, _require_dict_key(rev_op, "operation_id", "revoke kp1"))["state"] == "succeeded"

            still_up = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
            assert still_up.get("lifecycle") == "enabled", (
                f"Active plugin on kp2 must remain enabled when kp1 revoked: {still_up!r}"
            )

            p2_dis = disable_plugin(ctx, pid)
            try:
                rb_prep_op = ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/upgrades/prepare",
                    json_body={
                        "package_hash": pkg1.sha256_hex,
                        "version": "1.0.0",
                        "state_policy": "preserve",
                        "target_config": {"schema_revision": schema_rev, "values": {}, "secrets": {}},
                    },
                    if_match=str(p2_dis["revision"]),
                    idempotency_key=uniq("t3p04rbprep"),
                )
                rb_done = poll_operation(ctx, _require_dict_key(rb_prep_op, "operation_id", "rb prepare"))
                if rb_done["state"] == "succeeded":
                    rb_prep = rb_done.get("result") or rb_done
                    rb_digest = _extract_target_config_digest(rb_prep)
                    assert_api_error(
                        lambda: ctx.call_phase1(
                            "POST",
                            f"/api/v1/plugins/{pid}/rollback",
                            json_body={
                                "package_hash": pkg1.sha256_hex,
                                "preparation_id": rb_prep["preparation_id"],
                                "consent_token": rb_prep["consent_token"],
                                "grants": extract_prep_grants(rb_prep),
                                "state_policy": "preserve",
                                "target_config_digest": rb_digest,
                            },
                            if_match=str(p2_dis["revision"]),
                            idempotency_key=uniq("t3p04rb"),
                        ),
                        (403, 409),
                        label="rollback to revoked-key package",
                    )
            except ApiError as exc:
                assert exc.status in (403, 409), f"Unexpected rollback error: {exc}"
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P05",
        tier=3,
        feature_id=5,
        name="Staged install SIGKILL crash recovery + idempotent replay + lifecycle enable & audit",
        self_test=False,
        related_features=(5, 6, 8),
    )
    def test_t3_p05_staged_install_crash_enable_audit(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("t3p05"))
        pid = f"org.example.{uniq('t3p05')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sign_cmplugin_sha256(pkg.bytes, kp, hashed=True))
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up["upload_id"]},
            if_match="0",
            idempotency_key=uniq("t3p05prep"),
        )
        prep_done = poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "prepare"))
        assert prep_done["state"] == "succeeded"
        prep = prep_done.get("result") or prep_done
        grants = extract_prep_grants(prep)
        idem_key = uniq("t3p05idem")
        inst_body = {
            "preparation_id": _require_dict_key(prep, "preparation_id", "prep"),
            "consent_token": _require_dict_key(prep, "consent_token", "prep"),
            "grants": grants,
        }
        try:
            inst_op = ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations",
                json_body=inst_body,
                if_match="0",
                idempotency_key=idem_key,
            )
            op_id = str(_require_dict_key(inst_op, "operation_id", "install"))
            restart_and_relogin(ctx, kill=True)
            op_after = ctx.call_phase1("GET", f"/api/v1/plugin-operations/{op_id}")
            if op_after.get("state") not in ("succeeded", "failed", "cancelled"):
                assert "revision" in op_after, f"Missing revision on operation: {op_after!r}"
                try:
                    ctx.call_phase1(
                        "POST",
                        f"/api/v1/plugin-operations/{op_id}/resume",
                        json_body={},
                        if_match=str(op_after["revision"]),
                        idempotency_key=uniq("t3p05res"),
                    )
                except ApiError as exc:
                    if exc.status != 409:
                        raise
            done = poll_operation(ctx, op_id)
            assert done["state"] == "succeeded", f"Expected install recovery succeeded, got {done!r}"
            p_inst = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
            assert p_inst.get("lifecycle") == "installed-disabled"
            assert int(p_inst.get("generation", -1)) == 0

            replay = ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations",
                json_body=inst_body,
                if_match="0",
                idempotency_key=idem_key,
            )
            assert replay.get("operation_id") == op_id, f"Idempotent replay mismatch: {replay!r}"

            p_en = enable_plugin(ctx, pid, revision=int(p_inst["revision"]))
            assert p_en.get("lifecycle") == "enabled" and int(p_en.get("generation", 0)) >= 1
            audit = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/audit?limit=50"), "audit")
            assert len(audit) >= 1
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P06",
        tier=3,
        feature_id=6,
        name="Lifecycle drain gate + state_schema 1->2 upgrade (preserve blocked vs reset+reauth) + rollback",
        self_test=False,
        related_features=(6, 7, 5),
    )
    def test_t3_p06_lifecycle_upgrade_schema_reset_rollback(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('t3p06')}"
        try:
            pkg1, kp, p1 = setup_signed_plugin_ready(
                ctx, plugin_id=pid, version="1.0.0", state_schema=1, kind="hook", enable=True
            )
            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            schema_rev = str(cfg0["schema_revision"])
            pkg2 = build_cmplugin(plugin_id=pid, version="2.0.0", state_schema=2, kind="hook")
            up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True))

            # preserve across state_schema 1->2 must be rejected
            try:
                bad_prep = ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/upgrades/prepare",
                    json_body={
                        "upload_id": up2["upload_id"],
                        "version": "2.0.0",
                        "state_policy": "preserve",
                        "target_config": {"schema_revision": schema_rev, "values": {}, "secrets": {}},
                    },
                    if_match=str(p1["revision"]),
                    idempotency_key=uniq("t3p06badp"),
                )
                bad_done = poll_operation(ctx, _require_dict_key(bad_prep, "operation_id", "preserve prepare"))
                assert bad_done["state"] in ("failed", "rejected"), (
                    f"Expected preserve across state_schema change rejected, got {bad_done!r}"
                )
            except ApiError as exc:
                assert exc.status in (400, 409, 422), f"Unexpected error: {exc}"

            # upgrades/prepare while enabled succeeds with requires_drain=True + requires_reauth=True
            wrong_reauth = obtain_reauth(ctx, purpose="plugin:upload")
            en_prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "2.0.0",
                    "state_policy": "reset",
                    "target_config": {
                        "schema_revision": schema_rev,
                        "values": {"dry_run": False, "threshold": 88},
                        "secrets": {},
                    },
                },
                if_match=str(p1["revision"]),
                idempotency_key=uniq("t3p06enprep"),
            )
            en_prep = (poll_operation(ctx, _require_dict_key(en_prep_op, "operation_id", "en prepare")).get("result")) or {}
            en_impact = en_prep.get("impact") or {}
            assert en_impact.get("requires_drain") is True, (
                f"Expected requires_drain=True when preparing upgrade on enabled plugin: {en_prep!r}"
            )
            assert en_impact.get("requires_reauth") is True, (
                f"Expected requires_reauth=True for reset upgrade: {en_prep!r}"
            )

            p_dis = disable_plugin(ctx, pid)
            prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "2.0.0",
                    "state_policy": "reset",
                    "target_config": {
                        "schema_revision": schema_rev,
                        "values": {"dry_run": False, "threshold": 88},
                        "secrets": {},
                    },
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t3p06resetp"),
            )
            prep_done = poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "reset prepare"))
            assert prep_done["state"] == "succeeded"
            prep = prep_done.get("result") or prep_done
            digest = _extract_target_config_digest(prep)
            grants = extract_prep_grants(prep)

            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/upgrade",
                    json_body={
                        "preparation_id": prep["preparation_id"],
                        "consent_token": prep["consent_token"],
                        "grants": grants,
                        "state_policy": "reset",
                        "target_config_digest": digest,
                        "reauth_token": wrong_reauth,
                    },
                    if_match=str(p_dis["revision"]),
                    idempotency_key=uniq("t3p06badauth"),
                ),
                (401, 403, 409, 422),
                label="upgrade reset with wrong-purpose reauth_token",
            )

            reauth_up = obtain_reauth(ctx, purpose="plugin:upgrade")
            up_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": grants,
                    "state_policy": "reset",
                    "target_config_digest": digest,
                    "reauth_token": reauth_up,
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t3p06exec"),
            )
            assert poll_operation(ctx, _require_dict_key(up_op, "operation_id", "upgrade exec"))["state"] == "succeeded"
            p2 = enable_plugin(ctx, pid)
            assert p2.get("version") == "2.0.0"
            cfg2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            assert (cfg2.get("values") or {}).get("threshold") == 88

            p2_dis = disable_plugin(ctx, pid)
            rb_prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "package_hash": pkg1.sha256_hex,
                    "version": "1.0.0",
                    "state_policy": "reset",
                    "target_config": {
                        "schema_revision": schema_rev,
                        "values": {"dry_run": True, "threshold": 10},
                        "secrets": {},
                    },
                },
                if_match=str(p2_dis["revision"]),
                idempotency_key=uniq("t3p06rbp"),
            )
            rb_prep = (poll_operation(ctx, _require_dict_key(rb_prep_op, "operation_id", "rb prep")).get("result")) or {}
            rb_digest = _extract_target_config_digest(rb_prep)
            reauth_rb = obtain_reauth(ctx, purpose="plugin:rollback")
            rb_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/rollback",
                json_body={
                    "package_hash": pkg1.sha256_hex,
                    "preparation_id": rb_prep["preparation_id"],
                    "consent_token": rb_prep["consent_token"],
                    "grants": extract_prep_grants(rb_prep),
                    "state_policy": "reset",
                    "target_config_digest": rb_digest,
                    "reauth_token": reauth_rb,
                },
                if_match=str(p2_dis["revision"]),
                idempotency_key=uniq("t3p06rb"),
            )
            assert poll_operation(ctx, _require_dict_key(rb_op, "operation_id", "rollback"))["state"] == "succeeded"
            p_after = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
            assert p_after.get("version") == "1.0.0"
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P07",
        tier=3,
        feature_id=7,
        name="Schema and permission evolution (grant_diff, config_diff, target_config_digest tamper guard)",
        self_test=False,
        related_features=(7, 3, 8),
    )
    def test_t3_p07_upgrade_grant_diff_and_digest_tamper(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('t3p07')}"
        try:
            pkg1, kp, p1 = setup_signed_plugin_ready(
                ctx,
                plugin_id=pid,
                version="1.0.0",
                kind="hook",
                hooks=[{"id": "search.after", "priority": 400}],
                hook_permissions=["hooks:search.after"],
                schema_override=build_secret_config_schema(),
                enable=False,
            )
            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pid}/config",
                json_body={
                    "schema_revision": cfg0["schema_revision"],
                    "values": {"profile_label": "init", "dry_run": True, "threshold": 10},
                    "secrets": {"api_token": {"action": "replace", "value": "keep-secret-v1"}},
                },
                if_match=str(cfg0["revision"]),
                idempotency_key=uniq("t3p07cfg"),
            )
            p1_latest = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")

            evolved_schema = build_secret_config_schema()
            evolved_schema["properties"]["mode"] = {"type": "string", "default": "safe"}
            target_schema_rev = hashlib.sha256(
                json.dumps(evolved_schema, separators=(",", ":"), sort_keys=True, ensure_ascii=False).encode("utf-8")
            ).hexdigest()
            pkg2 = build_cmplugin(
                plugin_id=pid,
                version="1.1.0",
                kind="hook",
                hooks=[{"id": "search.after", "priority": 400}],
                hook_permissions=["hooks:search.after", "kv"],
                schema_override=evolved_schema,
            )
            up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True))
            prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "1.1.0",
                    "state_policy": "preserve",
                    "target_config": {
                        "schema_revision": target_schema_rev,
                        "values": {
                            "profile_label": "init",
                            "dry_run": True,
                            "threshold": 10,
                            "mode": "safe",
                        },
                        "secrets": {},
                    },
                },
                if_match=str(p1_latest["revision"]),
                idempotency_key=uniq("t3p07prep"),
            )
            prep = (poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "upgrades/prepare")).get("result")) or {}
            assert "grant_diff" in prep and "kv" in str(prep["grant_diff"]), (
                f"Expected grant_diff with added kv, got {prep!r}"
            )
            digest = _extract_target_config_digest(prep)
            grants = extract_prep_grants(prep)

            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/upgrade",
                    json_body={
                        "preparation_id": prep["preparation_id"],
                        "consent_token": prep["consent_token"],
                        "grants": grants,
                        "state_policy": "preserve",
                        "target_config_digest": "0" * 64,
                    },
                    if_match=str(p1_latest["revision"]),
                    idempotency_key=uniq("t3p07tamp"),
                ),
                (400, 409, 422),
                label="tampered target_config_digest",
            )

            up_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": grants,
                    "state_policy": "preserve",
                    "target_config_digest": digest,
                },
                if_match=str(p1_latest["revision"]),
                idempotency_key=uniq("t3p07exec"),
            )
            assert poll_operation(ctx, _require_dict_key(up_op, "operation_id", "upgrade"))["state"] == "succeeded"
            p2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
            assert p2.get("version") == "1.1.0"
            cfg2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            cfg2_secs = _require_dict_key(cfg2, "secrets", "GET /config post-upgrade")
            cfg2_tok = _require_dict_key(cfg2_secs, "api_token", "GET /config post-upgrade secrets")
            assert isinstance(cfg2_tok, dict) and cfg2_tok.get("configured") is True
            assert "keep-secret-v1" not in str(cfg2)
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P08",
        tier=3,
        feature_id=8,
        name="Purpose-bound /plugin-reauth + If-Match CAS + Member RBAC across trust keys and uploads",
        self_test=False,
        related_features=(8, 4, 2),
    )
    def test_t3_p08_reauth_cas_rbac_cross_purpose(ctx: TestContext) -> None:
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-reauth",
                json_body={"password": "wrong-admin-password", "purpose": "plugin:upload"},
            ),
            (401,),
            label="wrong password on /plugin-reauth",
        )
        tok_up = obtain_reauth(ctx, purpose="plugin:upload")
        tok_tk = obtain_reauth(ctx, purpose="trust-key:create")
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        prep = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-trust-keys/prepare",
            json_body={"public_key": kp.public_key_text, "label": uniq("t3p08")},
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugin-trust-keys",
                json_body={
                    "public_key": kp.public_key_text,
                    "fingerprint": _require_dict_key(prep, "fingerprint", "trust-keys/prepare"),
                    "consent_token": _require_dict_key(prep, "consent_token", "trust-keys/prepare"),
                    "reauth_token": tok_up,
                    "label": uniq("badpurp"),
                },
                if_match="0",
                idempotency_key=uniq("t3p08badtk"),
            ),
            (401, 403),
            label="plugin:upload token on trust-key:create",
        )
        pid = f"org.example.{uniq('t3p08')}"
        pkg = build_cmplugin(plugin_id=pid, kind="hook")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        assert_api_error(
            lambda: upload_cmplugin(ctx, pkg.bytes, sig_text=sig, reauth_token=tok_tk),
            (401, 403),
            label="trust-key:create token on plugin-uploads",
        )

        pin_trust_key(ctx, kp, label=uniq("t3p08ok"))
        try:
            prepare_and_install_plugin(ctx, pkg, kp=kp, pin_key=False)
            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/enable",
                    json_body={},
                    idempotency_key=uniq("t3p08noim"),
                ),
                (428,),
                label="enable missing If-Match",
            )
            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/enable",
                    json_body={},
                    if_match="999",
                    idempotency_key=uniq("t3p08badim"),
                ),
                (409,),
                label="enable stale If-Match",
            )

            _, mc = create_member_client(ctx)
            for label, fn in (
                ("member /plugin-reauth", lambda: mc.post("/api/v1/plugin-reauth", json_body={"password": "x", "purpose": "plugin:upload"})),
                ("member /plugin-trust-keys", lambda: mc.get("/api/v1/plugin-trust-keys")),
                ("member /plugins", lambda: mc.get("/api/v1/plugins")),
                ("member /config/validate", lambda: mc.post(f"/api/v1/plugins/{pid}/config/validate", json_body={"values": {}})),
            ):
                assert_api_error(fn, (403,), label=label)
        finally:
            _safe_cleanup_plugin(ctx, pid)

    # -----------------------------------------------------------------------
    # P2 Pairwise Combinations (T3-P09 .. T3-P13)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T3-P09",
        tier=3,
        feature_id=9,
        name="Atomic RegistrySnapshot tracks lifecycle transitions installed-disabled -> enabled -> disabled -> enabled",
        self_test=False,
        related_features=(9, 6, 11),
    )
    def test_t3_p09_registry_snapshot_lifecycle_transitions(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pid = f"org.example.{uniq('t3p09')}"
        ref = f"plugin:{pid}/e2e-demo"
        try:
            _, _, p0 = setup_signed_plugin_ready(
                ctx, plugin_id=pid, kind="yaml-only", site_id="e2e-demo", enable=False
            )
            cat0 = unwrap_list(ctx.call_phase1("GET", "/api/v1/sites/catalog"), "sites catalog")
            assert not any(c.get("profile_id") == ref for c in cat0 if isinstance(c, dict)), (
                f"Disabled plugin {ref} must not appear in /sites/catalog: {cat0!r}"
            )

            enable_plugin(ctx, pid, revision=int(p0["revision"]))
            cat1 = unwrap_list(ctx.call_phase1("GET", "/api/v1/sites/catalog"), "sites catalog")
            assert any(c.get("profile_id") == ref for c in cat1 if isinstance(c, dict)), (
                f"Enabled plugin {ref} missing from /sites/catalog: {cat1!r}"
            )
            site = ensure_site(ctx, pt.url, profile_id=ref)
            res1 = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
            assert "E2E.Demo.Release" in str(res1) and len(pt.request_log) >= 1

            reqs_before = len(pt.request_log)
            disable_plugin(ctx, pid)
            res2 = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
            items2 = res2.get("items") if isinstance(res2, dict) else res2
            assert items2 == [] or not any(
                it.get("site_id") == site["id"] for it in (items2 or []) if isinstance(it, dict)
            )
            assert len(pt.request_log) == reqs_before, (
                f"Disabled provider must not hit PT site: {pt.request_log[reqs_before:]!r}"
            )

            enable_plugin(ctx, pid)
            res3 = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
            assert "E2E.Demo.Release" in str(res3)
            assert len(pt.request_log) > reqs_before
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P10",
        tier=3,
        feature_id=10,
        name="YAML/WASM provider + net-guard same-origin redirects vs cross-origin & SSRF CredentialSlot isolation",
        self_test=False,
        related_features=(10, 9, 11),
    )
    def test_t3_p10_net_guard_provider_redirects_and_ssrf(ctx: TestContext) -> None:
        pt_a = ctx.new_pt_site(expected_cookie="uid=42; pass=secretA")
        pt_b = ctx.new_pt_site()
        qb = ctx.new_qbit()
        pid = f"org.example.{uniq('t3p10')}"
        try:
            setup_signed_plugin_ready(
                ctx, plugin_id=pid, kind="yaml-only", site_id="e2e-demo", enable=True
            )
            site_a = ensure_site(
                ctx,
                pt_a.url,
                profile_id=f"plugin:{pid}/e2e-demo",
                cookie="uid=42; pass=secretA",
            )
            dl = ensure_downloader(ctx, qb.url)
            sub = ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "site_id": site_a["id"],
                    "download_url": f"{pt_a.url}/redirect/3",
                    "title": "Same Origin Redirect",
                },
                idempotency_key=uniq("t3p10ok"),
            )
            if isinstance(sub, dict) and sub.get("operation_id"):
                assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
            assert len(qb.torrents) == 1
            assert all("uid=42" in r["headers"].get("cookie", "") for r in pt_a.request_log)

            for bad_url, lbl in (
                (f"{pt_a.url}/redirect/to?url={pt_b.url}/spy", "cross-origin redirect"),
                ("http://169.254.169.254/a.torrent", "metadata SSRF"),
            ):
                assert_api_error(
                    lambda u=bad_url: ctx.call_phase1(
                        "POST",
                        "/api/v1/downloaders/submit",
                        json_body={
                            "downloader_id": dl["id"],
                            "site_id": site_a["id"],
                            "download_url": u,
                            "title": "Blocked Target",
                        },
                        idempotency_key=uniq("t3p10bad"),
                    ),
                    (400, 403, 502),
                    label=lbl,
                )
            assert len(pt_b.spy_requests) == 0
            assert len(qb.torrents) == 1
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P11",
        tier=3,
        feature_id=11,
        name="indexer-plugin + maintenance-plugin + (site_id, credential_epoch) cookie rotation CAS",
        self_test=False,
        related_features=(11, 10, 13),
    )
    def test_t3_p11_indexer_maintenance_credential_epoch_cas(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=v1")
        pt.checkin_rotate_cookie = "pass=v2"
        maint_pid = f"org.example.{uniq('t3p11m')}"
        idx_pid = f"org.example.{uniq('t3p11i')}"
        try:
            ensure_plugin_enabled(ctx, "builtin.checkin")
            setup_signed_plugin_ready(ctx, plugin_id=maint_pid, kind="maintenance", enable=True)
            setup_signed_plugin_ready(ctx, plugin_id=idx_pid, kind="indexer", enable=True)
            site = ensure_site(
                ctx,
                pt.url,
                profile_id=f"plugin:{idx_pid}/fixture",
                cookie="uid=42; pass=v1",
            )
            act = ctx.call_phase1(
                "POST",
                f"/api/v1/sites/{site['id']}/plugin-actions",
                json_body={"action": "check-in", "provider_id": f"plugin:{maint_pid}/e2e-maint"},
                if_match=str(site["revision"]),
                idempotency_key=uniq("t3p11chk"),
            )
            if isinstance(act, dict) and act.get("operation_id"):
                assert poll_operation(ctx, act["operation_id"])["state"] == "succeeded"
            st1 = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
            assert any(
                isinstance(r, dict)
                and r.get("action") == "check-in"
                and r.get("plugin_id") == maint_pid
                and r.get("provider_id") == f"plugin:{maint_pid}/e2e-maint"
                and r.get("state") == "completed"
                for r in (st1.get("receipts") or [])
            ), f"Expected completed plugin check-in receipt in {st1!r}"
            pt_demo = ctx.new_pt_site(expected_cookie="uid=42; pass=v1")
            pt_demo.checkin_rotate_cookie = "pass=v2"
            demo_site = ensure_site(ctx, pt_demo.url, profile_id="demo", cookie="uid=42; pass=v1")
            demo_chk = ctx.call_phase1("POST", f"/api/v1/sites/{demo_site['id']}/check-in", json_body={})
            if isinstance(demo_chk, dict) and demo_chk.get("operation_id"):
                assert poll_operation(ctx, demo_chk["operation_id"])["state"] == "succeeded"
            assert pt_demo.checkin_post_count == 1
            assert isinstance(st1, dict) and st1.get("credential_epoch"), f"Missing credential_epoch: {st1!r}"

            ctx.call_phase1(
                "PATCH",
                f"/api/v1/sites/{site['id']}",
                json_body={"cookie": "uid=42; pass=v3"},
            )
            pt.expected_cookie = "uid=42; pass=v3"
            st2 = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
            assert isinstance(st2, dict) and st2.get("credential_epoch"), f"Missing credential_epoch: {st2!r}"
            assert str(st2["credential_epoch"]) != str(st1["credential_epoch"]), (
                f"Expected credential_epoch rotation: {st1!r} -> {st2!r}"
            )
            ver = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
            assert isinstance(ver, dict) and ver.get("ok") is True
            assert "pass=v3" in pt.request_log[-1]["headers"].get("cookie", "")
            assert "pass=v3" not in str(ver) and "pass=v3" not in str(st2)
        finally:
            _safe_cleanup_plugin(ctx, idx_pid)
            _safe_cleanup_plugin(ctx, maint_pid)

    @registry.case(
        id="T3-P12",
        tier=3,
        feature_id=12,
        name="Narrow BrowserSession triple-gate: built-in vs untrusted 3rd-party maintenance-plugin",
        self_test=False,
        related_features=(12, 11, 6),
    )
    def test_t3_p12_narrow_browser_triple_gate(ctx: TestContext) -> None:
        pt1 = ctx.new_pt_site()
        pt2 = ctx.new_pt_site()
        cdp = ctx.new_cdp()
        pid_3p = f"org.example.{uniq('t3p12')}"
        try:
            ensure_plugin_enabled(ctx, "builtin.checkin")
            put_browser_settings(ctx, True, cdp.url)
            _, _, p_3p = setup_signed_plugin_ready(ctx, plugin_id=pid_3p, kind="maintenance", enable=True)
            site1 = ensure_site(ctx, pt1.url, profile_id="demo")
            site2 = ensure_site(ctx, pt2.url, profile_id="demo")
            for s in (site1, site2):
                ctx.call_phase1("PATCH", f"/api/v1/sites/{s['id']}", json_body={"maintenance_via": "browser"})

            avail_3p = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/{pid_3p}/browser-availability?site_id={site1['id']}&purpose=maintenance",
            )
            assert avail_3p.get("status") != "ready"
            assert (
                "browser-isolation-unavailable" in str(avail_3p).lower()
                or bool(avail_3p.get("missing_requirements"))
            )
            try:
                chk = ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid_3p}/browser-checks",
                    json_body={"site_id": site1["id"], "purpose": "maintenance"},
                    if_match=str(p_3p["revision"]),
                    idempotency_key=uniq("t3p12untr"),
                )
                done = poll_operation(ctx, chk["operation_id"])
                assert done["state"] in ("failed", "rejected", "blocked"), (
                    f"Expected untrusted 3rd-party browser-check failure, got {done!r}"
                )
            except ApiError as exc:
                assert exc.status in (403, 409, 422), (
                    f"Expected 403/409/422 on untrusted 3rd-party browser-checks, got {exc}"
                )
            assert len(cdp.rpc_log) == 0

            avail_no_perm = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/builtin.checkin/browser-availability?site_id={site2['id']}&purpose=maintenance",
            )
            assert avail_no_perm.get("status") != "ready"
            assert avail_no_perm.get("missing_requirements")

            grant_site_browser_permission(ctx, "builtin.checkin", site1["id"], "browser:maintenance")
            avail_ok = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/builtin.checkin/browser-availability?site_id={site1['id']}&purpose=maintenance",
            )
            assert avail_ok.get("status") == "ready", f"Expected ready, got {avail_ok!r}"
            assert cdp.url not in str(avail_ok)
            p_chk = ctx.call_phase1("GET", "/api/v1/plugins/builtin.checkin")
            chk_op = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/builtin.checkin/browser-checks",
                json_body={"site_id": site1["id"], "purpose": "maintenance"},
                if_match=str(p_chk["revision"]),
                idempotency_key=uniq("t3p12ok"),
            )
            assert poll_operation(ctx, _require_dict_key(chk_op, "operation_id", "browser-checks"))["state"] == "succeeded"
            assert len(cdp.disposed_contexts) >= 1 and len(cdp.active_contexts) == 0
        finally:
            _safe_restore_browser(ctx)
            _safe_cleanup_plugin(ctx, pid_3p)

    @registry.case(
        id="T3-P13",
        tier=3,
        feature_id=13,
        name="BrowserSession check-in + site_maintenance_receipts window dedup + site.checkin.after outbox",
        self_test=False,
        related_features=(13, 12, 21),
    )
    def test_t3_p13_browser_checkin_dedup_outbox(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        cdp = ctx.new_cdp()
        hook_pid = f"org.example.{uniq('t3p13')}"
        try:
            ensure_plugin_enabled(ctx, "builtin.checkin")
            put_browser_settings(ctx, True, cdp.url)
            setup_signed_plugin_ready(
                ctx,
                plugin_id=hook_pid,
                kind="hook",
                hooks=[{"id": "site.checkin.after", "priority": 400}],
                hook_permissions=["hooks:site.checkin.after", "events:site.checkin.after", "http", "kv"],
                http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
                enable=True,
            )
            site = ensure_site(ctx, pt.url, profile_id="pterclub")
            ctx.call_phase1("PATCH", f"/api/v1/sites/{site['id']}", json_body={"maintenance_via": "browser"})
            grant_site_browser_permission(ctx, "builtin.checkin", site["id"], "browser:maintenance")

            r1 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
            if isinstance(r1, dict) and r1.get("operation_id"):
                assert poll_operation(ctx, r1["operation_id"])["state"] == "succeeded"
            assert pt.checkin_post_count == 0
            assert len(cdp.navigated_urls) >= 1
            disposed_after_1 = len(cdp.disposed_contexts)
            assert disposed_after_1 >= 1 and len(cdp.active_contexts) == 0
            _wait_for_webhooks(pt, min_count=1)
            assert len(pt.webhook_events) == 1

            r2 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
            if isinstance(r2, dict) and r2.get("operation_id"):
                r2 = poll_operation(ctx, r2["operation_id"])
            assert pt.checkin_post_count == 0
            assert len(cdp.disposed_contexts) == disposed_after_1, "Same-window check-in must deduplicate"
            assert len(pt.webhook_events) == 1
            status = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
            assert "already" in str(r2).lower() or "already" in str(status).lower() or "completed" in str(status).lower()
        finally:
            _safe_restore_browser(ctx)
            _safe_cleanup_plugin(ctx, hook_pid)

    # -----------------------------------------------------------------------
    # P3 & P4 Pairwise Combinations (T3-P14 .. T3-P18)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T3-P14",
        tier=3,
        feature_id=14,
        name="RegistrySnapshot (yaml-only + indexer-plugin) feeds search.before and search.after priority chain",
        self_test=False,
        related_features=(14, 9, 11),
    )
    def test_t3_p14_snapshot_and_search_hook_chain(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents = [
            {
                "id": "101",
                "title": "A",
                "size": "1.00 GiB",
                "size_bytes": 1073741824,
                "seeders": 10,
                "leechers": 1,
                "snatched": 1,
                "upload_time": "2026-11-20 12:00:00",
                "category": "Movie",
                "free": True,
                "hr": False,
            },
            {
                "id": "102",
                "title": "B",
                "size": "1.00 GiB",
                "size_bytes": 1073741824,
                "seeders": 10,
                "leechers": 1,
                "snatched": 1,
                "upload_time": "2026-11-20 12:00:00",
                "category": "Movie",
                "free": True,
                "hr": False,
            },
        ]
        pid_y = f"org.example.{uniq('t3p14y')}"
        pid_sb = f"org.example.{uniq('t3p14sb')}"
        pid_sa1 = f"org.example.{uniq('t3p14sa1')}"
        pid_sa2 = f"org.example.{uniq('t3p14sa2')}"
        try:
            setup_signed_plugin_ready(ctx, plugin_id=pid_y, kind="yaml-only", site_id="e2e-demo", enable=True)
            setup_signed_plugin_ready(
                ctx, plugin_id=pid_sb, kind="hook", hooks=[{"id": "search.before", "priority": 100}], enable=True
            )
            setup_signed_plugin_ready(
                ctx, plugin_id=pid_sa1, kind="hook", hooks=[{"id": "search.after", "priority": 100}], enable=True
            )
            setup_signed_plugin_ready(
                ctx, plugin_id=pid_sa2, kind="hook", hooks=[{"id": "search.after", "priority": 200}], enable=True
            )
            ensure_site(ctx, pt.url, profile_id=f"plugin:{pid_y}/e2e-demo")
            res = ctx.call_phase1("GET", "/api/v1/search/torrents")
            items = res.get("items") if isinstance(res, dict) else res
            assert isinstance(items, list), f"Expected list items, got {res!r}"
            titles = [it.get("title") for it in items if isinstance(it, dict)]
            assert "B" in titles and "A" not in titles, f"Expected B kept and A dropped, got {titles!r}"
            assert len(pt.request_log) >= 1
            assert "search.before" in str(unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid_sb}/audit?limit=50"), "sb audit"))
            assert "search.after" in str(unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid_sa1}/audit?limit=50"), "sa audit"))
        finally:
            _safe_cleanup_plugin(ctx, pid_sa2)
            _safe_cleanup_plugin(ctx, pid_sa1)
            _safe_cleanup_plugin(ctx, pid_sb)
            _safe_cleanup_plugin(ctx, pid_y)

    @registry.case(
        id="T3-P15",
        tier=3,
        feature_id=15,
        name="Subscribe workflow hooks + multi-file verified .torrent parser + dishonest 1TiB rejection",
        self_test=False,
        related_features=(15, 14, 16),
    )
    def test_t3_p15_hooks_verified_torrent_and_dishonest_reject(ctx: TestContext) -> None:
        tf = build_torrent_bytes(
            name="E2E.Demo.Movie.2026",
            files=[(["movie.mkv"], 2000000), (["sub.srt"], 500000)],
        )
        dishonest = build_dishonest_torrent(actual_bytes=1024 ** 4)
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.default_torrent = tf
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        boost_tf = build_torrent_bytes(name="T3.P15.Boost", length=2500000)
        pt.torrent_payloads["boost15"] = boost_tf.bytes
        pt.torrent_payloads["dishonest"] = dishonest.bytes
        qb = ctx.new_qbit(disk_free_bytes=10 * 1024 ** 3)
        hook_pids: List[str] = []
        try:
            dl = ensure_downloader(ctx, qb.url)
            site = ensure_site(ctx, pt.url, profile_id="demo")
            lib = ensure_library(ctx)
            sub = create_subscribe(ctx, lib["id"])
            ensure_plugin_enabled(ctx, "builtin.boost")
            for hid in ("filter.after", "subscribe.score", "subscribe.select"):
                hpid = f"org.example.{uniq('t3p15')}"
                setup_signed_plugin_ready(
                    ctx,
                    plugin_id=hpid,
                    kind="hook",
                    hooks=[{"id": hid, "priority": 200}],
                    enable=True,
                    tracked_pids=hook_pids,
                )

            ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
            assert tf.info_hash in qb.torrents, f"Expected multi-file torrent {tf.info_hash} in qB: {qb.torrents!r}"
            assert qb.torrents[tf.info_hash]["size"] == 2500000
            adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
            assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]

            boost_op = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/builtin.boost/commands",
                json_body={
                    "kind": "downloader.add",
                    "payload": {
                        "downloader_id": dl["id"],
                        "site_id": site["id"],
                        "download_url": f"{pt.url}/torrents/boost15.torrent",
                        "title": "T3 P15 Boost",
                    },
                },
                idempotency_key=uniq("t3p15b"),
            )
            assert poll_operation(ctx, boost_op["operation_id"])["state"] == "succeeded"
            state = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/builtin.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
            )
            assert int(_require_dict_key(state, "used_bytes", "brush-state")) >= 2500000

            try:
                bad_op = ctx.call_phase1(
                    "POST",
                    "/api/v1/plugins/builtin.boost/commands",
                    json_body={
                        "kind": "downloader.add",
                        "payload": {
                            "downloader_id": dl["id"],
                            "site_id": site["id"],
                            "download_url": f"{pt.url}/torrents/dishonest.torrent",
                            "title": "Dishonest 1TiB",
                            "advertised_size_bytes": 1,
                            "budget_bytes": 10 * 1024 ** 3,
                        },
                    },
                    idempotency_key=uniq("t3p15dish"),
                )
                bad_done = poll_operation(ctx, _require_dict_key(bad_op, "operation_id", "dishonest add"))
                assert bad_done["state"] in ("failed", "rejected")
            except ApiError as exc:
                assert exc.status in (400, 409, 422)
            assert len(qb.torrents) == 2
            state2 = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/builtin.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
            )
            assert int(_require_dict_key(state2, "reserved_bytes", "brush-state")) == 0
        finally:
            for hpid in hook_pids:
                _safe_cleanup_plugin(ctx, hpid)

    @registry.case(
        id="T3-P16",
        tier=3,
        feature_id=16,
        name="download.before reject/defer vs DownloadPlanPatch + verified bytes + download_operations/reservations",
        self_test=False,
        related_features=(16, 14, 15),
    )
    def test_t3_p16_download_before_patch_and_reservations(ctx: TestContext) -> None:
        tf = build_torrent_bytes(name="Patched.Release.1080p", length=4194304)
        pt = ctx.new_pt_site()
        pt.default_torrent = tf
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "builtin.boost")
        hook_pid = f"org.example.{uniq('t3p16')}"
        try:
            setup_signed_plugin_ready(
                ctx,
                plugin_id=hook_pid,
                kind="hook",
                hooks=[{"id": "download.before", "priority": 100}],
                enable=True,
            )
            try:
                r_rej = ctx.call_phase1(
                    "POST",
                    "/api/v1/downloaders/submit",
                    json_body={
                        "downloader_id": dl["id"],
                        "site_id": site["id"],
                        "download_url": f"{pt.url}/download.torrent",
                        "title": "RejectDownload",
                    },
                    idempotency_key=uniq("t3p16rej"),
                )
                if isinstance(r_rej, dict) and r_rej.get("operation_id"):
                    d_rej = poll_operation(
                        ctx,
                        r_rej["operation_id"],
                        terminal_states=("rejected", "deferred", "failed", "succeeded"),
                    )
                    assert d_rej["state"] in ("rejected", "deferred", "failed"), (
                        f"Expected download.before reject/defer, got {d_rej!r}"
                    )
                else:
                    assert isinstance(r_rej, dict) and r_rej.get("outcome") in ("rejected", "deferred", "failed"), (
                        f"Expected rejected/deferred outcome, got {r_rej!r}"
                    )
            except ApiError as exc:
                assert exc.status in (400, 403, 409, 422)
            assert [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"] == []
            st_rej = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/builtin.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
            )
            assert int(_require_dict_key(st_rej, "reserved_bytes", "brush-state")) == 0
            audit = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{hook_pid}/audit?limit=50"), "audit")
            assert "download.before" in str(audit)

            r_ok = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/builtin.boost/commands",
                json_body={
                    "kind": "downloader.add",
                    "payload": {
                        "downloader_id": dl["id"],
                        "site_id": site["id"],
                        "download_url": f"{pt.url}/download.torrent",
                        "title": "Patched Release",
                    },
                },
                idempotency_key=uniq("t3p16ok"),
            )
            if isinstance(r_ok, dict) and r_ok.get("operation_id"):
                assert poll_operation(ctx, r_ok["operation_id"])["state"] == "succeeded"
            assert tf.info_hash in qb.torrents
            assert qb.torrents[tf.info_hash]["size"] == 4194304
            st_ok = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/builtin.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
            )
            assert int(st_ok["reserved_bytes"]) == 0
            assert int(st_ok["used_bytes"]) >= 4194304
            tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
            assert any(t.get("info_hash") == tf.info_hash for t in tasks if isinstance(t, dict))
        finally:
            _safe_cleanup_plugin(ctx, hook_pid)

    @registry.case(
        id="T3-P17",
        tier=3,
        feature_id=17,
        name="Unified DownloadSubmitter unknown reconciliation across restart + reservation release + download.after outbox",
        self_test=False,
        related_features=(17, 16, 21),
    )
    def test_t3_p17_unknown_reconcile_and_outbox(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        qb.drop_response_on_next_add = True
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "builtin.boost")
        hook_pid = f"org.example.{uniq('t3p17')}"
        try:
            setup_signed_plugin_ready(
                ctx,
                plugin_id=hook_pid,
                kind="hook",
                hooks=[{"id": "download.after", "priority": 400}],
                hook_permissions=["hooks:download.after", "events:download.after", "http", "kv"],
                http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
                enable=True,
            )
            try:
                res = ctx.call_phase1(
                    "POST",
                    "/api/v1/plugins/builtin.boost/commands",
                    json_body={
                        "kind": "downloader.add",
                        "payload": {
                            "downloader_id": dl["id"],
                            "site_id": site["id"],
                            "download_url": f"{pt.url}/download.torrent",
                            "title": "Reconcile Outbox",
                        },
                    },
                    idempotency_key=uniq("t3p17sub"),
                )
                if isinstance(res, dict) and res.get("operation_id"):
                    done = poll_operation(
                        ctx, res["operation_id"], terminal_states=("unknown", "succeeded", "failed")
                    )
                    assert done["state"] in ("unknown", "succeeded")
            except ApiError as exc:
                assert exc.status in (400, 500, 502, 504)
            restart_and_relogin(ctx, kill=True)
            adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
            assert len(adds) == 1 and len(qb.torrents) == 1
            st = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/builtin.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
            )
            assert int(_require_dict_key(st, "reserved_bytes", "brush-state")) == 0
            assert int(_require_dict_key(st, "used_bytes", "brush-state")) >= pt.default_torrent.verified_total_bytes
            _wait_for_webhooks(pt, min_count=1)
            assert len(pt.webhook_events) >= 1
        finally:
            _safe_cleanup_plugin(ctx, hook_pid)

    @registry.case(
        id="T3-P18",
        tier=3,
        feature_id=18,
        name="boost.snapshot (used+reserved+retained_file_bytes) + foreign-hash isolation + 2-step cleanup plan",
        self_test=False,
        related_features=(18, 16, 17),
    )
    def test_t3_p18_boost_snapshot_foreign_and_cleanup(ctx: TestContext) -> None:
        save_dir = ctx.tmp_dir / "t3p18_qb"
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit(real_save_dir=save_dir)
        fh = "f" * 40
        qb.seed_foreign_torrent(fh, name="foreign.keep", size=4096, tags="manual")
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ensure_plugin_enabled(ctx, "builtin.boost")

        r_leg = ctx.client.raw_request("GET", "/api/v1/sites/boost-stats")
        assert r_leg.status == 404, f"Legacy /sites/boost-stats must be 404, got {r_leg.status}"

        add_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "T3P18 Boost",
                },
            },
            idempotency_key=uniq("t3p18add"),
        )
        assert poll_operation(ctx, _require_dict_key(add_op, "operation_id", "downloader.add"))["state"] == "succeeded"
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        owned = [
            t
            for t in tasks
            if isinstance(t, dict)
            and t.get("downloader_id") == dl["id"]
            and t.get("info_hash") == pt.default_torrent.info_hash
        ]
        assert len(owned) == 1, f"Expected owned task in {tasks!r}"
        assert not any(t.get("info_hash") == fh for t in tasks if isinstance(t, dict))
        tid = owned[0]["id"]

        del_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            json_body={"kind": "downloader.delete-task", "payload": {"task_id": tid}},
            idempotency_key=uniq("t3p18del"),
        )
        assert poll_operation(ctx, _require_dict_key(del_op, "operation_id", "delete-task"))["state"] == "succeeded"
        assert any(d["deleteFiles"] is False for d in qb.deleted_log)
        assert list(save_dir.glob("**/*.mkv"))
        st1 = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/builtin.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(_require_dict_key(st1, "retained_file_bytes", "brush-state")) > 0

        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            json_body={
                "kind": "downloader.prepare-cleanup",
                "payload": {"site_id": site["id"], "downloader_id": dl["id"], "task_ids": [tid]},
            },
            idempotency_key=uniq("t3p18prep"),
        )
        prep_done = poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "prepare-cleanup"))
        assert prep_done["state"] == "succeeded"
        plan_handle = (prep_done.get("result") or {}).get("plan_handle")
        assert plan_handle, f"Missing plan_handle: {prep_done!r}"

        prev = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/builtin.boost/brush-cleanup-preview?plan_handle={plan_handle}",
        )
        assert prev.get("mode") == "plan" and prev.get("confirmation_token")
        df_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            json_body={
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": plan_handle,
                    "revision": prev.get("revision", 1),
                    "confirmation_token": prev["confirmation_token"],
                    "reason": "t3p18 cleanup",
                },
            },
            idempotency_key=uniq("t3p18df"),
        )
        assert poll_operation(ctx, _require_dict_key(df_op, "operation_id", "delete-files"))["state"] == "succeeded"
        st2 = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/builtin.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(st2["retained_file_bytes"]) == 0
        assert not list(save_dir.glob("**/*.mkv"))
        assert fh in qb.torrents

    # -----------------------------------------------------------------------
    # P5 Core Pairwise Combinations (T3-P19 .. T3-P21 & T3-P29)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T3-P19",
        tier=3,
        feature_id=19,
        name="DownloadSubmitter -> qB completion -> transfer.rename before LibraryWorkLease -> transfer.completed",
        self_test=False,
        related_features=(19, 14, 17),
    )
    def test_t3_p19_submit_to_transfer_rename_and_completed(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        save_dir = ctx.tmp_dir / uniq("t3p19_qb")
        save_dir.mkdir(parents=True, exist_ok=True)
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit(real_save_dir=save_dir)
        pid_ren = f"org.example.{uniq('t3p19r')}"
        pid_cmp = f"org.example.{uniq('t3p19c')}"
        try:
            setup_signed_plugin_ready(
                ctx, plugin_id=pid_ren, kind="hook", hooks=[{"id": "transfer.rename", "priority": 200}], enable=True
            )
            setup_signed_plugin_ready(
                ctx,
                plugin_id=pid_cmp,
                kind="hook",
                hooks=[{"id": "transfer.completed", "priority": 400}],
                hook_permissions=["hooks:transfer.completed", "events:transfer.completed", "http", "kv"],
                http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
                enable=True,
            )
            ensure_downloader(ctx, qb.url)
            ensure_site(ctx, pt.url, profile_id="demo")
            sub = create_subscribe(ctx, lib["id"])
            ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
            assert len(qb.torrents) >= 1, f"Expected torrent in qB, got {qb.torrents!r}"
            with qb._lock:
                for h, t in qb.torrents.items():
                    t["state"] = "uploading"
                    t["progress"] = 1.0
                    t["downloaded"] = t["size"]
                    for f in qb.files.get(h, []):
                        f["progress"] = 1.0
            trigger_job(ctx, "transfer")
            items = unwrap_list(ctx.call_phase1("GET", f"/api/v1/libraries/{lib['id']}/items"), "library items")
            assert len(items) >= 1
            audit = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid_ren}/audit?limit=50"), "audit")
            assert "transfer.rename" in str(audit)
            _wait_for_webhooks(pt, min_count=1)
            assert len(pt.webhook_events) >= 1
        finally:
            _safe_cleanup_plugin(ctx, pid_cmp)
            _safe_cleanup_plugin(ctx, pid_ren)

    @registry.case(
        id="T3-P20",
        tier=3,
        feature_id=20,
        name="Transfer relocation + ADR-0012 library removal fencing + orphan/favorite purge + zero spurious events",
        self_test=False,
        related_features=(20, 19, 21),
    )
    def test_t3_p20_transfer_adr0012_purge_and_silence(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        _keep_lib = ensure_library(ctx, kind="movie", name=uniq("t3p20k"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("t3p20d"))
        ctx.call_phase1("PUT", f"/api/v1/libraries/{del_lib['id']}/default")
        pid_ren = f"org.example.{uniq('t3p20r')}"
        pid_cmp = f"org.example.{uniq('t3p20c')}"
        prev_watch: Optional[str] = None
        watch_set = False
        try:
            setup_signed_plugin_ready(
                ctx, plugin_id=pid_ren, kind="hook", hooks=[{"id": "transfer.rename", "priority": 200}], enable=True
            )
            setup_signed_plugin_ready(
                ctx,
                plugin_id=pid_cmp,
                kind="hook",
                hooks=[{"id": "transfer.completed", "priority": 400}],
                hook_permissions=["hooks:transfer.completed", "events:transfer.completed", "http", "kv"],
                http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
                enable=True,
            )
            watch_dir = ctx.tmp_dir / "t3p20_watch"
            watch_dir.mkdir(parents=True, exist_ok=True)
            (watch_dir / "Fenced.Movie.2026.1080p.mkv").write_bytes(b"fake-t3p20-mkv")
            prev_watch = _set_watch_intake(ctx, watch_dir)
            watch_set = True
            trigger_job(ctx, "watch_intake")
            _set_watch_intake(ctx, Path(prev_watch) if prev_watch else None)
            watch_set = False

            items = unwrap_list(ctx.call_phase1("GET", f"/api/v1/libraries/{del_lib['id']}/items"), "del_lib items")
            assert len(items) >= 1
            media_id = _require_dict_key(items[0], "media_item_id", "library item")
            detail = ctx.call_phase1("GET", f"/api/v1/libraries/{del_lib['id']}/items/{media_id}")
            files_list = _require_dict_key(detail, "files", "item detail")
            assert isinstance(files_list, list) and len(files_list) >= 1, f"Expected files in {detail!r}"
            disk_file = Path(_require_dict_key(files_list[0], "path", "item detail file"))
            _wait_for_webhooks(pt, min_count=1)
            assert len(pt.webhook_events) == 1

            ctx.call_phase1(
                "POST",
                "/api/v1/playback/marks",
                json_body={"media_item_id": media_id, "favorite": True},
            )
            del_op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
            op_id = _require_dict_key(del_op, "operation_id", "DELETE library")
            libs_now = unwrap_list(ctx.call_phase1("GET", "/api/v1/libraries"), "libraries")
            assert not any(l.get("id") == del_lib["id"] for l in libs_now if isinstance(l, dict))
            assert_api_error(
                lambda: ctx.call_phase1("GET", f"/api/v1/libraries/{del_lib['id']}/items"),
                (404, 409),
                label="fenced library items read",
            )
            assert_api_error(
                lambda: ctx.call_phase1("POST", f"/api/v1/libraries/{del_lib['id']}/scan", json_body={}),
                (404, 409),
                label="fenced library scan",
            )
            assert poll_operation_removal(ctx, op_id)["state"] == "succeeded"
            favs = unwrap_items(ctx.call_phase1("GET", "/api/v1/playback/favorites"), "favorites")
            up_next = unwrap_items(ctx.call_phase1("GET", "/api/v1/playback/up-next"), "up-next")
            assert not any(f.get("media_item_id") == media_id for f in favs if isinstance(f, dict))
            assert not any(u.get("media_item_id") == media_id for u in up_next if isinstance(u, dict))
            assert disk_file.is_file(), f"ADR-0012 deletion must preserve media file {disk_file}"
            assert len(pt.webhook_events) == 1
        finally:
            if watch_set:
                _safe_restore_watch_intake(ctx, prev_watch)
            _safe_cleanup_plugin(ctx, pid_cmp)
            _safe_cleanup_plugin(ctx, pid_ren)

    @registry.case(
        id="T3-P21",
        tier=3,
        feature_id=21,
        name="Per-DB outbox (app + subscribe + library) + rollback 0 events + SIGKILL webhook retry dedup",
        self_test=False,
        related_features=(21, 13, 19),
    )
    def test_t3_p21_three_db_outbox_rollback_and_crash_dedup(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit()
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        obs_pid = f"org.example.{uniq('t3p21')}"
        prev_watch: Optional[str] = None
        watch_set = False
        try:
            setup_signed_plugin_ready(
                ctx,
                plugin_id=obs_pid,
                kind="hook",
                hooks=[
                    {"id": "site.checkin.after", "priority": 400},
                    {"id": "download.after", "priority": 400},
                    {"id": "transfer.completed", "priority": 400},
                ],
                hook_permissions=[
                    "hooks:site.checkin.after",
                    "events:site.checkin.after",
                    "hooks:download.after",
                    "events:download.after",
                    "hooks:transfer.completed",
                    "events:transfer.completed",
                    "http",
                    "kv",
                ],
                http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
                enable=True,
            )
            # 1. Rollback emits 0 events
            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    "/api/v1/downloaders/submit",
                    json_body={
                        "downloader_id": dl["id"],
                        "download_url": "http://169.254.169.254/fail.torrent",
                        "title": "Rollback Outbox",
                    },
                    idempotency_key=uniq("t3p21rb"),
                ),
                (400, 403, 422),
                label="SSRF submit rollback",
            )
            assert len(pt.webhook_events) == 0

            # 2. app.db outbox: site.checkin.after
            ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
            _wait_for_webhooks(pt, min_count=1)
            assert len(pt.webhook_events) == 1
            raw_ev1 = pt.webhook_events[0]
            ev1 = raw_ev1.get("body") if isinstance(raw_ev1, dict) and isinstance(raw_ev1.get("body"), dict) else raw_ev1
            assert isinstance(ev1, dict) and (ev1.get("id") or ev1.get("event_id"))

            # 3. subscribe.db outbox + webhook failure + SIGKILL restart retry dedup
            pt.webhook_fail_next = 1
            sub_res = ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "Outbox Crash Dedup",
                },
                idempotency_key=uniq("t3p21sub"),
            )
            if isinstance(sub_res, dict) and sub_res.get("operation_id"):
                assert poll_operation(ctx, str(sub_res["operation_id"]))["state"] == "succeeded"
            restart_and_relogin(ctx, kill=True)
            _wait_for_webhooks(pt, min_count=2)
            assert len(pt.webhook_events) == 2

            # 4. library.db outbox: transfer.completed
            watch_dir = ctx.tmp_dir / "t3p21_watch"
            watch_dir.mkdir(parents=True, exist_ok=True)
            (watch_dir / "Outbox.T3P21.2026.1080p.mkv").write_bytes(b"fake-t3p21-mkv")
            prev_watch = _set_watch_intake(ctx, watch_dir)
            watch_set = True
            trigger_job(ctx, "watch_intake")
            _set_watch_intake(ctx, Path(prev_watch) if prev_watch else None)
            watch_set = False
            _wait_for_webhooks(pt, min_count=3)
            assert len(pt.webhook_events) == 3
        finally:
            if watch_set:
                _safe_restore_watch_intake(ctx, prev_watch)
            _safe_cleanup_plugin(ctx, obs_pid)

    # -----------------------------------------------------------------------
    # P6 & Cross-Phase Pairwise Combinations (T3-P22 .. T3-P29)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T3-P22",
        tier=3,
        feature_id=22,
        name="Author CLI scaffold --ui react / build / pack / sign -> MF contract 1.0.0 -> plugin_ui_trust & cookie assets",
        self_test=False,
        related_features=(22, 23, 24),
    )
    def test_t3_p22_cli_to_mf_ui_trust_assets(ctx: TestContext) -> None:
        contract = ctx.call_phase1("GET", "/api/v1/plugin-ui-contract")
        assert contract.get("contract") == "1.0.0" and contract.get("react") == "19.3.0"

        proj = ctx.tmp_dir / "t3p22_proj"
        out_pkg = ctx.tmp_dir / "t3p22.cmplugin"
        pid = f"org.example.{uniq('t3p22')}"
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        key_file = ctx.tmp_dir / "t3p22.key"
        key_file.write_text(kp.secret_seed.hex(), encoding="utf-8")

        assert run_plugin_cli(
            ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", "--ui", "react", str(proj)]
        ).returncode == 0
        toml_path = proj / "plugin.toml"
        assert toml_path.is_file(), f"Expected plugin.toml at {toml_path}"
        _rewrite_toml_fields(toml_path, {"id": pid, "version": "1.0.0"})
        assert run_plugin_cli(ctx, ["validate", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["build", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["pack", str(proj), "--output", str(out_pkg)]).returncode == 0
        assert out_pkg.is_file()
        assert run_plugin_cli(ctx, ["sign", str(out_pkg), "--key", str(key_file)]).returncode == 0
        sig_path = out_pkg.with_suffix(".cmplugin.minisig")
        assert sig_path.is_file(), f"Missing .minisig at {sig_path}"

        pkg_bytes = out_pkg.read_bytes()
        sig_text = sig_path.read_text(encoding="utf-8")
        assert verify_minisign(hashlib.sha256(pkg_bytes).digest(), sig_text, kp.public_key_text).valid

        pin_trust_key(ctx, kp, label=uniq("t3p22k"))
        try:
            up = upload_cmplugin(ctx, pkg_bytes, sig_text=sig_text)
            prep_op = ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations/prepare",
                json_body={"upload_id": up["upload_id"]},
                if_match="0",
                idempotency_key=uniq("t3p22prep"),
            )
            prep = (poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "prepare")).get("result")) or {}
            inst_op = ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": extract_prep_grants(prep),
                },
                if_match="0",
                idempotency_key=uniq("t3p22inst"),
            )
            assert poll_operation(ctx, _require_dict_key(inst_op, "operation_id", "install"))["state"] == "succeeded"
            enable_plugin(ctx, pid)
            grant_ui_trust(ctx, pid, up["hash"])
            desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
            assert desc.get("contract") == "1.0.0" and desc.get("react") == "19.3.0"
            asset_r = ctx.client.clone_cookie_only().get_asset(desc["entry_url"])
            assert asset_r.status == 200 and len(asset_r.body) > 0
            assert asset_r.header("x-content-type-options") == "nosniff"
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P23",
        tier=3,
        feature_id=23,
        name="Author CLI canonical validator parity with server & dev-mode sideload vs unsigned rejection",
        self_test=False,
        related_features=(23, 1, 2, 3),
    )
    def test_t3_p23_cli_validator_parity_and_dev_sideload(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        proj = ctx.tmp_dir / "t3p23_proj"
        out_pkg = ctx.tmp_dir / "t3p23.cmplugin"
        pid = f"org.example.{uniq('t3p23')}"
        assert run_plugin_cli(ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", str(proj)]).returncode == 0
        toml_path = proj / "plugin.toml"
        schema_path = proj / "schemas" / "config.schema.json"
        assert toml_path.is_file() and schema_path.is_file()
        _rewrite_toml_fields(toml_path, {"id": pid})

        orig_schema = schema_path.read_text(encoding="utf-8")
        bad_schema = {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": False,
            "$defs": {"node": {"$ref": "#/$defs/node"}},
            "properties": {"loop": {"$ref": "#/$defs/node"}},
        }
        schema_path.write_text(json.dumps(bad_schema, indent=2), encoding="utf-8")
        assert run_plugin_cli(ctx, ["validate", str(proj)]).returncode != 0
        assert run_plugin_cli(ctx, ["pack", str(proj), "--output", str(out_pkg)]).returncode != 0
        assert not out_pkg.exists()

        schema_path.write_text(orig_schema, encoding="utf-8")
        assert run_plugin_cli(ctx, ["build", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["pack", str(proj), "--output", str(out_pkg)]).returncode == 0
        assert out_pkg.is_file() and out_pkg.stat().st_size > 22

        _safe_restore_developer_mode(ctx, False)
        assert run_plugin_cli(ctx, ["dev", str(proj), "--instance", ctx.base_url]).returncode != 0
        assert_api_error(
            lambda: upload_cmplugin(ctx, out_pkg.read_bytes(), sig_text=None),
            (401, 403),
            label="unsigned upload while dev mode off",
        )

        try:
            set_developer_mode(ctx, True)
            assert run_plugin_cli(ctx, ["dev", str(proj), "--instance", ctx.base_url]).returncode == 0
            plugins = _list_all_paginated(ctx, "/api/v1/plugins", "plugins list")
            assert any(p.get("id") == pid for p in plugins if isinstance(p, dict)), (
                f"Dev-sideloaded plugin {pid} missing in {plugins!r}"
            )
        finally:
            _safe_cleanup_plugin(ctx, pid)
            _safe_restore_developer_mode(ctx, False)

    @registry.case(
        id="T3-P24",
        tier=3,
        feature_id=24,
        name="plugin_ui_trust + Cookie-only asset auth + ui/query redaction + X-Plugin-{Package,Generation} + RBAC",
        self_test=False,
        related_features=(24, 25, 8),
    )
    def test_t3_p24_ui_trust_cookie_query_and_generation_headers(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=ui-secret-999")
        site = ensure_site(ctx, pt.url, profile_id="pterclub", cookie="uid=42; pass=ui-secret-999")
        pid = f"org.example.{uniq('t3p24')}"
        try:
            pkg, _, plugin = setup_signed_plugin_ready(
                ctx,
                plugin_id=pid,
                kind="ui-only",
                ui_overrides={"capabilities": ["read:sites", "command:indexer.search"]},
                enable=True,
            )
            me = ctx.client.me()
            cookie_cli = ctx.client.clone_cookie_only()
            desc = cookie_cli.get(f"/api/v1/plugins/{pid}/ui")
            assert desc.get("subject_user_id") == me.get("id")
            entry_url = _require_dict_key(desc, "entry_url", "UiDescriptor")

            assert cookie_cli.get_asset(entry_url).status == 403
            grant_ui_trust(ctx, pid, pkg.sha256_hex)
            r_ok = cookie_cli.get_asset(entry_url, use_cookie=True, no_bearer=True)
            assert r_ok.status == 200
            assert r_ok.header("x-content-type-options") == "nosniff"
            assert "no-store" in (r_ok.header("cache-control") or "").lower()

            bearer_only = ctx.client.clone(token=ctx.client.token, session_cookie=None)
            assert bearer_only.raw_request("GET", entry_url, no_cookie=True).status == 401
            _, mc = create_member_client(ctx)
            assert mc.get_asset(entry_url, use_cookie=True, no_bearer=True).status == 403

            resolved = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/ui/query",
                json_body={"name": "handles.resolve", "input": {"kind": "site", "id": site["id"]}},
            )
            assert isinstance(resolved, dict) and resolved.get("handle")
            assert "ui-secret-999" not in str(resolved)

            cmd_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/commands",
                json_body={"kind": "indexer.search", "payload": {"site_handle": resolved["handle"], "keyword": "E2E"}},
                plugin_package=pkg.sha256_hex,
                plugin_generation=str(plugin["generation"]),
                idempotency_key=uniq("t3p24cmd"),
            )
            assert cmd_op.get("operation_id")

            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/commands",
                    json_body={"kind": "indexer.search", "payload": {"site_handle": resolved["handle"], "keyword": "E2E"}},
                    plugin_package=pkg.sha256_hex,
                    plugin_generation="999",
                    idempotency_key=uniq("t3p24forged"),
                ),
                (400, 403, 409),
                label="forged X-Plugin-Generation",
            )
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P25",
        tier=3,
        feature_id=25,
        name="/plugin-apps/:id/:surfaceId vs Jellyfin /plugins/* collision + 4 surfaces + lifecycle editability",
        self_test=False,
        related_features=(25, 22, 24),
    )
    def test_t3_p25_route_separation_four_surfaces_and_editability(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('t3p25')}"
        try:
            setup_signed_plugin_ready(
                ctx,
                plugin_id=pid,
                kind="ui-only",
                ui_overrides={
                    "surfaces": [
                        {"id": "cfg", "kind": "config", "expose": "./Config", "title": "Config", "icon": "sliders"},
                        {"id": "pg", "kind": "page", "expose": "./Page", "title": "Page", "icon": "file-text"},
                        {"id": "app", "kind": "app", "expose": "./App", "title": "App", "icon": "puzzle"},
                        {"id": "dash", "kind": "dashboard", "expose": "./Dashboard", "title": "Dash", "icon": "activity"},
                    ]
                },
                enable=False,
            )
            jf = ctx.client.raw_request("GET", f"/plugins/{pid}/app", no_bearer=True, no_cookie=True)
            assert jf.status == 404
            assert "application/json" in (jf.header("content-type") or "").lower()

            if ctx.supervisor is not None and ctx.web_dir is None:
                web_dir = make_temp_web_dir(ctx)
                with InstanceSupervisor(
                    bin_path=resolve_bin_path(ctx),
                    web_dir=web_dir,
                    cli_token=ctx.cli_token,
                    admin_password=ctx.admin_password,
                    keep_tmp=ctx.keep_tmp,
                ) as ui_sup:
                    app_r = ui_sup.client().raw_request(
                        "GET", f"/plugin-apps/{pid}/app", no_bearer=True, no_cookie=True
                    )
                    assert app_r.status == 200
                    assert "text/html" in (app_r.header("content-type") or "").lower()
                    html_txt = app_r.body.decode("utf-8", errors="ignore").lower()
                    assert '<div id="root"' in html_txt or "<div id='root'" in html_txt or "<script" in html_txt
            else:
                app_r = ctx.client.raw_request(
                    "GET", f"/plugin-apps/{pid}/app", no_bearer=True, no_cookie=True
                )
                assert app_r.status == 200
                assert "text/html" in (app_r.header("content-type") or "").lower()
                html_txt = app_r.body.decode("utf-8", errors="ignore").lower()
                assert '<div id="root"' in html_txt or "<div id='root'" in html_txt or "<script" in html_txt

            desc0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
            surfs0 = {s["kind"]: s for s in _require_dict_key(desc0, "surfaces", "UiDescriptor")}
            assert set(("config", "page", "app", "dashboard")).issubset(surfs0.keys()), (
                f"Expected all 4 surfaces in {surfs0!r}"
            )
            assert surfs0["config"]["editable"] is True
            assert all(surfs0[k]["mountable"] is False for k in ("page", "app", "dashboard"))

            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pid}/config",
                json_body={"schema_revision": cfg0["schema_revision"], "values": {"dry_run": False, "threshold": 15}, "secrets": {}},
                if_match=str(cfg0["revision"]),
                idempotency_key=uniq("t3p25cfg"),
            )

            enable_plugin(ctx, pid)
            pkg_hash = _require_dict_key(desc0, "package_hash", "UiDescriptor")
            grant_ui_trust(ctx, pid, pkg_hash)
            desc1 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
            surfs1 = {s["kind"]: s for s in _require_dict_key(desc1, "surfaces", "UiDescriptor")}
            assert all(surfs1[k]["mountable"] is True for k in ("config", "page", "app", "dashboard"))
            assert surfs1["config"]["editable"] is False
            nav = unwrap_list(ctx.call_phase1("GET", "/api/v1/plugin-ui-navigation"), "navigation")
            assert any(n.get("plugin_id") == pid for n in nav if isinstance(n, dict))

            cfg1 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            assert_api_error(
                lambda: ctx.call_phase1(
                    "PUT",
                    f"/api/v1/plugins/{pid}/config",
                    json_body={"schema_revision": cfg1["schema_revision"], "values": {"dry_run": True, "threshold": 20}, "secrets": {}},
                    if_match=str(cfg1["revision"]),
                    idempotency_key=uniq("t3p25lock"),
                ),
                (409,),
                label="PUT /config after enable",
            )
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P26",
        tier=3,
        feature_id=26,
        name="First-party Check-in & Boost UIs + site_maintenance_receipts + boost.snapshot + legacy 404",
        self_test=False,
        related_features=(26, 13, 18),
    )
    def test_t3_p26_first_party_checkin_and_boost_uis(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        checkin = _find_first_party_plugin(ctx, ("builtin.checkin", "org.example.checkin"), "checkin")
        boost = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        checkin_id = checkin["id"]
        boost_id = boost["id"]

        r_stats = ctx.client.raw_request("GET", "/api/v1/sites/boost-stats")
        r_patch = ctx.client.raw_request(
            "PATCH",
            f"/api/v1/sites/{site['id']}/boost",
            body=b"{}",
            headers={"Content-Type": "application/json"},
        )
        assert r_stats.status == 404 and r_patch.status == 404

        ensure_plugin_enabled(ctx, checkin_id)
        ensure_plugin_enabled(ctx, boost_id)
        c_desc = ctx.call_phase1("GET", f"/api/v1/plugins/{checkin_id}/ui")
        b_desc = ctx.call_phase1("GET", f"/api/v1/plugins/{boost_id}/ui")
        grant_ui_trust(ctx, checkin_id, _require_dict_key(c_desc, "package_hash", "checkin ui"))
        grant_ui_trust(ctx, boost_id, _require_dict_key(b_desc, "package_hash", "boost ui"))

        ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{checkin_id}/ui/query",
            json_body={"name": "checkin.receipts", "input": {"site_id": site["id"]}},
        )
        assert pt.checkin_post_count == 0
        c_op1 = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{checkin_id}/commands",
            json_body={"kind": "site.maintenance", "payload": {"site_id": site["id"], "action": "check-in"}},
            idempotency_key=uniq("t3p26c1"),
        )
        assert poll_operation(ctx, _require_dict_key(c_op1, "operation_id", "checkin cmd 1"))["state"] == "succeeded"
        assert pt.checkin_post_count == 1
        c_op2 = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{checkin_id}/commands",
            json_body={"kind": "site.maintenance", "payload": {"site_id": site["id"], "action": "check-in"}},
            idempotency_key=uniq("t3p26c2"),
        )
        assert poll_operation(ctx, _require_dict_key(c_op2, "operation_id", "checkin cmd 2"))["state"] == "succeeded"
        assert pt.checkin_post_count == 1

        b_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "T3P26 Boost",
                },
            },
            idempotency_key=uniq("t3p26b"),
        )
        assert poll_operation(ctx, _require_dict_key(b_op, "operation_id", "boost add"))["state"] == "succeeded"
        snap = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/ui/query",
            json_body={"name": "boost.snapshot", "input": {"site_id": site["id"], "downloader_id": dl["id"]}},
        )
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{boost_id}/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert isinstance(snap, dict) and isinstance(state, dict)
        assert int(_require_dict_key(snap, "used_bytes", "boost.snapshot")) >= pt.default_torrent.verified_total_bytes
        assert snap["used_bytes"] == _require_dict_key(state, "used_bytes", "brush-state")
        assert _require_dict_key(snap, "retained_file_bytes", "boost.snapshot") == 0

    @registry.case(
        id="T3-P27",
        tier=3,
        feature_id=7,
        name="Plugin upgrade v1->v2 invalidates per-hash plugin_ui_trust and bumps generation for X-Plugin-Generation",
        self_test=False,
        related_features=(7, 24, 25),
    )
    def test_t3_p27_upgrade_invalidates_ui_trust_and_generation(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('t3p27')}"
        try:
            pkg1, kp, p1 = setup_signed_plugin_ready(
                ctx,
                plugin_id=pid,
                version="1.0.0",
                kind="ui-only",
                ui_overrides={"capabilities": ["read:sites", "command:indexer.search"]},
                enable=True,
            )
            grant_ui_trust(ctx, pid, pkg1.sha256_hex)
            cookie_cli = ctx.client.clone_cookie_only()
            desc1 = cookie_cli.get(f"/api/v1/plugins/{pid}/ui")
            assert (desc1.get("trust") or {}).get("status") == "trusted", f"Expected trusted v1, got {desc1!r}"
            assert cookie_cli.get_asset(desc1["entry_url"]).status == 200

            pkg2 = build_cmplugin(
                plugin_id=pid,
                version="1.1.0",
                kind="ui-only",
                ui_overrides={"capabilities": ["read:sites", "command:indexer.search"]},
                extra_ui_assets={"ui/assets/v2.js": (b"export const v = 2;\n", "application/javascript")},
            )
            assert pkg2.sha256_hex != pkg1.sha256_hex
            up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True))
            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            p_dis = disable_plugin(ctx, pid)
            prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "1.1.0",
                    "state_policy": "preserve",
                    "target_config": {"schema_revision": str(cfg0["schema_revision"]), "values": {}, "secrets": {}},
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t3p27prep"),
            )
            prep = (poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "upgrades/prepare")).get("result")) or {}
            digest = _extract_target_config_digest(prep)
            up_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": extract_prep_grants(prep),
                    "state_policy": "preserve",
                    "target_config_digest": digest,
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t3p27up"),
            )
            assert poll_operation(ctx, _require_dict_key(up_op, "operation_id", "upgrade"))["state"] == "succeeded"
            p2 = enable_plugin(ctx, pid)
            assert int(p2["generation"]) > int(p1["generation"])

            desc2 = cookie_cli.get(f"/api/v1/plugins/{pid}/ui")
            assert desc2.get("package_hash") == pkg2.sha256_hex
            assert (desc2.get("trust") or {}).get("status") != "trusted"
            assert cookie_cli.get_asset(desc2["entry_url"]).status == 403

            grant_ui_trust(ctx, pid, pkg2.sha256_hex)
            assert cookie_cli.get_asset(desc2["entry_url"]).status == 200

            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/commands",
                    json_body={"kind": "indexer.search", "payload": {"keyword": "E2E"}},
                    plugin_package=pkg2.sha256_hex,
                    plugin_generation=str(p1["generation"]),
                    idempotency_key=uniq("t3p27stale"),
                ),
                (400, 403, 409),
                label="stale X-Plugin-Generation after upgrade",
            )
        finally:
            _safe_cleanup_plugin(ctx, pid)

    @registry.case(
        id="T3-P28",
        tier=3,
        feature_id=8,
        name="Admin /plugin-reauth + ui-trust/grant + Boost 10-min prepare-cleanup + confirmation_token enforcement",
        self_test=False,
        related_features=(8, 18, 26),
    )
    def test_t3_p28_reauth_ui_trust_and_boost_confirmation_token(ctx: TestContext) -> None:
        save_dir = ctx.tmp_dir / "t3p28_qb"
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit(real_save_dir=save_dir)
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        boost = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        boost_id = boost["id"]
        ensure_plugin_enabled(ctx, boost_id)

        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{boost_id}/ui")
        pkg_hash = _require_dict_key(desc, "package_hash", "boost ui")
        prep_trust = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/ui-trust/prepare",
            json_body={"package_hash": pkg_hash},
        )
        wrong_reauth = obtain_reauth(ctx, purpose="plugin:upload")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{boost_id}/ui-trust/grant",
                json_body={
                    "package_hash": pkg_hash,
                    "consent_token": _require_dict_key(prep_trust, "consent_token", "ui-trust/prepare"),
                    "reauth_token": wrong_reauth,
                },
                if_match=str((prep_trust.get("trust_revision") if isinstance(prep_trust, dict) else 0) or 0),
                idempotency_key=uniq("t3p28badtr"),
            ),
            (401, 403),
            label="wrong reauth purpose on ui-trust/grant",
        )
        grant_ui_trust(ctx, boost_id, pkg_hash)

        add_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "T3P28 Cleanup Target",
                },
            },
            idempotency_key=uniq("t3p28add"),
        )
        assert poll_operation(ctx, _require_dict_key(add_op, "operation_id", "downloader.add"))["state"] == "succeeded"
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        matched = [
            t
            for t in tasks
            if isinstance(t, dict)
            and t.get("downloader_id") == dl["id"]
            and t.get("info_hash") == pt.default_torrent.info_hash
        ]
        assert len(matched) == 1
        tid = matched[0]["id"]

        del_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={"kind": "downloader.delete-task", "payload": {"task_id": tid}},
            idempotency_key=uniq("t3p28del"),
        )
        assert poll_operation(ctx, _require_dict_key(del_op, "operation_id", "delete-task"))["state"] == "succeeded"
        assert list(save_dir.glob("**/*.mkv"))

        sel_prev = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{boost_id}/brush-cleanup-preview?task_ids={tid}",
        )
        assert sel_prev.get("mode") == "selection" and sel_prev.get("confirmation_token") is None

        prep_cln = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={
                "kind": "downloader.prepare-cleanup",
                "payload": {"site_id": site["id"], "downloader_id": dl["id"], "task_ids": [tid]},
            },
            idempotency_key=uniq("t3p28prep"),
        )
        prep_done = poll_operation(ctx, _require_dict_key(prep_cln, "operation_id", "prepare-cleanup"))
        assert prep_done["state"] == "succeeded"
        plan_handle = (prep_done.get("result") or {}).get("plan_handle")
        assert plan_handle

        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{boost_id}/commands",
                json_body={
                    "kind": "downloader.delete-files",
                    "payload": {
                        "host_file_plan_handle": plan_handle,
                        "revision": 1,
                        "confirmation_token": "forged-token",
                        "reason": "forged",
                    },
                },
                idempotency_key=uniq("t3p28forged"),
            ),
            (400, 403, 409),
            label="forged confirmation_token on delete-files",
        )
        assert list(save_dir.glob("**/*.mkv"))

        plan_prev = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{boost_id}/brush-cleanup-preview?plan_handle={plan_handle}",
        )
        assert plan_prev.get("mode") == "plan" and plan_prev.get("confirmation_token")
        df_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": plan_handle,
                    "revision": plan_prev.get("revision", 1),
                    "confirmation_token": plan_prev["confirmation_token"],
                    "reason": "t3p28 valid cleanup",
                },
            },
            idempotency_key=uniq("t3p28df"),
        )
        assert poll_operation(ctx, _require_dict_key(df_op, "operation_id", "delete-files"))["state"] == "succeeded"
        assert not list(save_dir.glob("**/*.mkv"))
        snap = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/ui/query",
            json_body={"name": "boost.snapshot", "input": {"site_id": site["id"], "downloader_id": dl["id"]}},
        )
        assert _require_dict_key(snap, "retained_file_bytes", "boost.snapshot") == 0

    @registry.case(
        id="T3-P29",
        tier=3,
        feature_id=20,
        name="ADR-0012 library deletion detaches Subscribe.library_id to null while preserving download_ownership & brush-state",
        self_test=False,
        related_features=(20, 16, 17),
    )
    def test_t3_p29_library_deletion_preserves_download_ownership_and_brush(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit()
        _keep_lib = ensure_library(ctx, kind="movie", name=uniq("t3p29k"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("t3p29d"))
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        sub = create_subscribe(ctx, del_lib["id"])
        boost_plugin = ensure_plugin_enabled(ctx, "builtin.boost")

        boost_tf = build_torrent_bytes(name="T3P29.Boost.Item", length=4096)
        pt.torrent_payloads["boost"] = boost_tf.bytes
        ctx.call_phase1(
            "POST",
            "/api/v1/search/admit",
            json_body={
                "subscribe_id": sub["id"],
                "enclosure": f"{pt.url}/torrents/101.torrent",
            },
            idempotency_key=uniq("t3p29adm"),
        )
        assert pt.default_torrent.info_hash in qb.torrents

        boost_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/builtin.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/torrents/boost.torrent",
                    "title": "T3P29 Boost Item",
                },
            },
            if_match=str(boost_plugin["revision"]),
            idempotency_key=uniq("t3p29bst"),
        )
        assert poll_operation(ctx, _require_dict_key(boost_op, "operation_id", "boost add"))["state"] == "succeeded"
        assert boost_tf.info_hash in qb.torrents

        del_op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        assert poll_operation_removal(ctx, _require_dict_key(del_op, "operation_id", "DELETE library"))["state"] == "succeeded"

        sub_after = ctx.call_phase1("GET", f"/api/v1/subscriptions/{sub['id']}")
        assert isinstance(sub_after, dict) and sub_after.get("library_id") is None, (
            f"Expected library_id=None after library deletion, got {sub_after!r}"
        )
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        task_hashes = {t.get("info_hash") for t in tasks if isinstance(t, dict)}
        assert pt.default_torrent.info_hash in task_hashes and boost_tf.info_hash in task_hashes, (
            f"Expected download_ownership preserved in {tasks!r}"
        )
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/builtin.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(_require_dict_key(state, "used_bytes", "brush-state")) >= boost_tf.verified_total_bytes
