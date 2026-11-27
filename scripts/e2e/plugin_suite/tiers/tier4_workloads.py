"""Tier 4: Real-World End-to-End Application Scenarios.

Retains T4-INFRA-01 (self_test=True smoke check, with zero ctx.self_test
branches) and implements Scenarios 1-13 (T4-SCEN-01 through T4-SCEN-13,
self_test=False) matching TEST_INFRA.md.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import time
from typing import Any, Dict, List, Optional
import uuid

from ..lib.client import ApiError
from ..lib.fixtures import (
    build_cmplugin,
    build_dishonest_torrent,
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
    poll_library_operation,
    poll_operation,
    post_multipart_phase1,
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
from .tier3_combinations import (
    _extract_target_config_digest,
    _list_all_paginated,
    _require_dict_key,
    _rewrite_toml_fields,
    _safe_cleanup_plugin,
    _safe_restore_browser,
    _safe_restore_developer_mode,
    _safe_restore_watch_intake,
)


def _extract_event_ids(webhook_events: List[Dict[str, Any]]) -> List[str]:
    out: List[str] = []
    for item in webhook_events:
        if not isinstance(item, dict):
            continue
        body = item.get("body") if isinstance(item.get("body"), dict) else item
        ev_id = body.get("id") or body.get("event_id")
        if ev_id:
            out.append(str(ev_id))
    return out


def register_cases(registry: SuiteRegistry) -> None:
    @registry.case(
        id="T4-INFRA-01",
        tier=4,
        feature_id=6,
        name="End-to-end auth session lifecycle, RBAC enforcement, hook catalog, and SSE stream",
        self_test=True,
        related_features=(6, 8, 14),
    )
    def test_t4_session_rbac_and_sse_lifecycle(ctx: TestContext) -> None:
        # 1. Verify hook catalog readable by admin
        catalog = unwrap_items(ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog"), "hook catalog")
        assert len(catalog) >= 1

        # 2. Provision a member user and verify strict RBAC denial on admin plugin routes
        member_login = f"t4_member_{uuid.uuid4().hex[:8]}"
        member_pw = "member-t4-pw-999"
        created = ctx.client.create_member(member_login, member_pw)
        assert created.get("role") == "member"

        member_client = ctx.new_client()
        login_data = member_client.login(member_login, member_pw)
        assert login_data["user"]["id"] == created["id"]
        assert member_client.session_cookie, "Expected mc_session cookie from login"

        try:
            member_client.get("/api/v1/plugins")
            raise AssertionError("Expected 403 when member accesses GET /api/v1/plugins")
        except ApiError as exc:
            assert exc.status == 403, f"Expected 403, got {exc.status}"

        # 3. Verify member SSE stream works with authenticated client
        events = member_client.read_sse("/api/v1/jobs/stream", max_events=1, timeout=3.0)
        assert len(events) >= 1, f"Expected >= 1 SSE event from /jobs/stream, got {events!r}"
        assert events[0].get("event") in ("ready", "job"), (
            f"Unexpected SSE event from /jobs/stream: {events[0]!r}"
        )

        # 4. Logout invalidates token & session cookie
        member_client.logout()
        try:
            member_client.me()
            raise AssertionError("Expected 401 after logout")
        except ApiError as exc:
            assert exc.status == 401

    # -----------------------------------------------------------------------
    # Scenario 1: Signed Plugin Full Lifecycle
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-01",
        tier=4,
        feature_id=2,
        name="Scenario 1: Signed Plugin Full Lifecycle (CLI scaffold/pack/sign -> trust -> install -> config -> enable -> run -> upgrade -> uninstall)",
        self_test=False,
        related_features=(1, 2, 3, 4, 6, 7, 8, 23),
    )
    def test_t4_scen01_signed_plugin_full_lifecycle(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        key_file = ctx.tmp_dir / "t4s1_pub.key"
        key_file.write_text(kp.secret_seed.hex(), encoding="utf-8")
        pid = f"org.example.{uniq('t4s1')}"
        proj = ctx.tmp_dir / "t4s1_proj"
        pkg1_path = ctx.tmp_dir / "t4s1_v1.cmplugin"
        pkg2_path = ctx.tmp_dir / "t4s1_v2.cmplugin"

        assert run_plugin_cli(
            ctx, ["scaffold", "--world", "task-plugin", "--api", "1.0.0", str(proj)]
        ).returncode == 0
        toml_path = proj / "plugin.toml"
        assert toml_path.is_file(), f"Missing scaffolded plugin.toml at {toml_path}"
        _rewrite_toml_fields(toml_path, {"id": pid, "version": "1.0.0"})

        assert run_plugin_cli(ctx, ["validate", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["build", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["pack", str(proj), "--output", str(pkg1_path)]).returncode == 0
        assert pkg1_path.is_file()
        assert run_plugin_cli(ctx, ["sign", str(pkg1_path), "--key", str(key_file)]).returncode == 0
        sig1_path = pkg1_path.with_suffix(".cmplugin.minisig")
        assert sig1_path.is_file()

        pin_trust_key(ctx, kp, label=uniq("t4s1k"))
        try:
            up1 = upload_cmplugin(
                ctx, pkg1_path.read_bytes(), sig_text=sig1_path.read_text(encoding="utf-8")
            )
            assert up1.get("hash") == hashlib.sha256(pkg1_path.read_bytes()).hexdigest()
            assert up1.get("signature_status") in ("signed", "trusted", "verified")

            prep_op = ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations/prepare",
                json_body={"upload_id": up1["upload_id"]},
                if_match="0",
                idempotency_key=uniq("t4s1prep"),
            )
            prep = (poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "prepare")).get("result")) or {}
            inst_op = ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations",
                json_body={
                    "preparation_id": _require_dict_key(prep, "preparation_id", "prep"),
                    "consent_token": _require_dict_key(prep, "consent_token", "prep"),
                    "grants": extract_prep_grants(prep),
                },
                if_match="0",
                idempotency_key=uniq("t4s1inst"),
            )
            assert poll_operation(ctx, _require_dict_key(inst_op, "operation_id", "install"))["state"] == "succeeded"

            p1 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
            assert p1.get("lifecycle") == "installed-disabled"
            assert int(p1.get("revision", 0)) == 1
            assert p1.get("version") == "1.0.0"

            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            cfg_rev = int(_require_dict_key(cfg0, "revision", "GET /config"))
            values_v1 = dict(cfg0.get("values") or {})
            put_res = ctx.call_phase1(
                "PUT",
                f"/api/v1/plugins/{pid}/config",
                json_body={
                    "schema_revision": _require_dict_key(cfg0, "schema_revision", "GET /config"),
                    "values": values_v1,
                    "secrets": {},
                },
                if_match=str(cfg_rev),
                idempotency_key=uniq("t4s1cfg"),
            )
            assert int(put_res.get("revision", 0)) >= cfg_rev

            p_en = enable_plugin(ctx, pid)
            assert p_en.get("lifecycle") == "enabled" and int(p_en.get("generation", 0)) >= 1

            tasks = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/tasks"), "tasks")
            assert len(tasks) >= 1, f"Expected task in {tasks!r}"
            task_item = tasks[0]
            if task_item.get("enabled") is False:
                upd_op = ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/tasks/{task_item['id']}/update",
                    json_body={
                        "enabled": True,
                        "schedule": task_item.get("schedule") or {"interval_secs": 3600},
                        "config": task_item.get("config") or {},
                    },
                    if_match=str(task_item.get("definition_revision", p_en["revision"])),
                    idempotency_key=uniq("t4s1ten"),
                )
                assert poll_operation(ctx, _require_dict_key(upd_op, "operation_id", "task update"))["state"] == "succeeded"
                refreshed = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/tasks"), "tasks")
                task_item = next(
                    (t for t in refreshed if isinstance(t, dict) and t.get("id") == task_item["id"]),
                    task_item,
                )
            run_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/tasks/{task_item['id']}/run",
                json_body={"input": values_v1},
                if_match=str(task_item.get("definition_revision", p_en["revision"])),
                idempotency_key=uniq("t4s1run"),
            )
            assert poll_operation(ctx, _require_dict_key(run_op, "operation_id", "task run"))["state"] == "succeeded"
            audit = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/audit?limit=20"), "audit")
            assert len(audit) >= 1

            # Upgrade 1.0.0 -> 1.1.0 with state_policy="preserve"
            _rewrite_toml_fields(toml_path, {"id": pid, "version": "1.1.0"})
            assert run_plugin_cli(ctx, ["validate", str(proj)]).returncode == 0
            assert run_plugin_cli(ctx, ["build", str(proj)]).returncode == 0
            assert run_plugin_cli(ctx, ["pack", str(proj), "--output", str(pkg2_path)]).returncode == 0
            assert run_plugin_cli(ctx, ["sign", str(pkg2_path), "--key", str(key_file)]).returncode == 0
            sig2_path = pkg2_path.with_suffix(".cmplugin.minisig")

            up2 = upload_cmplugin(
                ctx, pkg2_path.read_bytes(), sig_text=sig2_path.read_text(encoding="utf-8")
            )
            p_dis = disable_plugin(ctx, pid)
            up_prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "1.1.0",
                    "state_policy": "preserve",
                    "target_config": {
                        "schema_revision": str(cfg0["schema_revision"]),
                        "values": values_v1,
                        "secrets": {},
                    },
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t4s1upprep"),
            )
            up_prep = (poll_operation(ctx, _require_dict_key(up_prep_op, "operation_id", "upgrades/prepare")).get("result")) or {}
            digest = _extract_target_config_digest(up_prep)
            reauth_up = obtain_reauth(ctx, purpose="plugin:upgrade")
            up_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": _require_dict_key(up_prep, "preparation_id", "up_prep"),
                    "consent_token": _require_dict_key(up_prep, "consent_token", "up_prep"),
                    "grants": extract_prep_grants(up_prep),
                    "state_policy": "preserve",
                    "target_config_digest": digest,
                    "reauth_token": reauth_up,
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t4s1up"),
            )
            assert poll_operation(ctx, _require_dict_key(up_op, "operation_id", "upgrade"))["state"] == "succeeded"
            p2 = enable_plugin(ctx, pid)
            assert p2.get("version") == "1.1.0"

            p2_dis = disable_plugin(ctx, pid)
            un_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/uninstall",
                json_body={"remove_private_data": True},
                if_match=str(p2_dis["revision"]),
                idempotency_key=uniq("t4s1un"),
            )
            assert poll_operation(ctx, _require_dict_key(un_op, "operation_id", "uninstall"))["state"] == "succeeded"
            r404 = ctx.client.raw_request("GET", f"/api/v1/plugins/{pid}")
            assert r404.status == 404, f"Expected 404 after uninstall, got {r404.status}"
        finally:
            _safe_cleanup_plugin(ctx, pid)

    # -----------------------------------------------------------------------
    # Scenario 2: Breaking Upgrade & Rollback
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-02",
        tier=4,
        feature_id=3,
        name="Scenario 2: Breaking Upgrade (state_schema=1 -> 2) & Rollback (v2 -> v1) with reauth & digest guard",
        self_test=False,
        related_features=(3, 5, 6, 7, 8),
    )
    def test_t4_scen02_breaking_upgrade_and_rollback(ctx: TestContext) -> None:
        pid = f"org.example.{uniq('t4s2')}"
        try:
            pkg1, kp, p1 = setup_signed_plugin_ready(
                ctx, plugin_id=pid, version="1.0.0", state_schema=1, kind="task", enable=True
            )
            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            schema_rev = str(cfg0["schema_revision"])
            tasks = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/tasks"), "tasks")
            assert len(tasks) >= 1
            task_item = tasks[0]
            if task_item.get("enabled") is False:
                upd_op = ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/tasks/{task_item['id']}/update",
                    json_body={
                        "enabled": True,
                        "schedule": task_item.get("schedule") or {"interval_secs": 3600},
                        "config": task_item.get("config") or {},
                    },
                    if_match=str(task_item.get("definition_revision", p1["revision"])),
                    idempotency_key=uniq("t4s2ten"),
                )
                assert poll_operation(ctx, _require_dict_key(upd_op, "operation_id", "task update"))["state"] == "succeeded"
                refreshed = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/tasks"), "tasks")
                task_item = next(
                    (t for t in refreshed if isinstance(t, dict) and t.get("id") == task_item["id"]),
                    task_item,
                )
            run1 = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/tasks/{task_item['id']}/run",
                json_body={"input": {}},
                if_match=str(task_item.get("definition_revision", p1["revision"])),
                idempotency_key=uniq("t4s2run1"),
            )
            assert poll_operation(ctx, _require_dict_key(run1, "operation_id", "task run 1"))["state"] == "succeeded"

            pkg2 = build_cmplugin(plugin_id=pid, version="2.0.0", state_schema=2, kind="task")
            up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True))

            p1_dis = disable_plugin(ctx, pid)
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
                    if_match=str(p1_dis["revision"]),
                    idempotency_key=uniq("t4s2pres"),
                )
                bad_done = poll_operation(ctx, _require_dict_key(bad_prep, "operation_id", "preserve prepare"))
                assert bad_done["state"] in ("failed", "rejected"), (
                    f"Expected preserve rejected across state_schema 1->2, got {bad_done!r}"
                )
            except ApiError as exc:
                assert exc.status in (400, 409, 422)
            assert ctx.call_phase1("GET", f"/api/v1/plugins/{pid}").get("version") == "1.0.0"

            target_cfg_v2 = {
                "schema_revision": schema_rev,
                "values": {"dry_run": False, "threshold": 77},
                "secrets": {},
            }
            prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "2.0.0",
                    "state_policy": "reset",
                    "target_config": target_cfg_v2,
                },
                if_match=str(p1_dis["revision"]),
                idempotency_key=uniq("t4s2resetp"),
            )
            prep_done = poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "reset prepare"))
            assert prep_done["state"] == "succeeded"
            prep = prep_done.get("result") or prep_done
            digest_v2 = _extract_target_config_digest(prep)
            grants_v2 = extract_prep_grants(prep)

            wrong_reauth = obtain_reauth(ctx, purpose="trust-key:create")
            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/upgrade",
                    json_body={
                        "preparation_id": _require_dict_key(prep, "preparation_id", "prep"),
                        "consent_token": _require_dict_key(prep, "consent_token", "prep"),
                        "grants": grants_v2,
                        "state_policy": "reset",
                        "target_config_digest": digest_v2,
                        "reauth_token": wrong_reauth,
                    },
                    if_match=str(p1_dis["revision"]),
                    idempotency_key=uniq("t4s2badauth"),
                ),
                (401, 403, 409, 422),
                label="wrong reauth purpose on breaking upgrade",
            )
            reauth_up = obtain_reauth(ctx, purpose="plugin:upgrade")
            assert_api_error(
                lambda: ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{pid}/upgrade",
                    json_body={
                        "preparation_id": prep["preparation_id"],
                        "consent_token": prep["consent_token"],
                        "grants": grants_v2,
                        "state_policy": "reset",
                        "target_config_digest": "f" * 64,
                        "reauth_token": reauth_up,
                    },
                    if_match=str(p1_dis["revision"]),
                    idempotency_key=uniq("t4s2baddig"),
                ),
                (400, 409, 422),
                label="tampered target_config_digest on breaking upgrade",
            )

            reauth_up = obtain_reauth(ctx, purpose="plugin:upgrade")
            up_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": grants_v2,
                    "state_policy": "reset",
                    "target_config_digest": digest_v2,
                    "reauth_token": reauth_up,
                },
                if_match=str(p1_dis["revision"]),
                idempotency_key=uniq("t4s2up"),
            )
            assert poll_operation(ctx, _require_dict_key(up_op, "operation_id", "upgrade"))["state"] == "succeeded"
            p2 = enable_plugin(ctx, pid)
            assert p2.get("version") == "2.0.0" and int(p2["generation"]) > int(p1["generation"])

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
                idempotency_key=uniq("t4s2rbp"),
            )
            rb_done = poll_operation(ctx, _require_dict_key(rb_prep_op, "operation_id", "rollback prepare"))
            assert rb_done["state"] == "succeeded"
            rb_prep = rb_done.get("result") or rb_done
            rb_digest = _extract_target_config_digest(rb_prep)
            reauth_rb = obtain_reauth(ctx, purpose="plugin:rollback")
            rb_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/rollback",
                json_body={
                    "package_hash": pkg1.sha256_hex,
                    "preparation_id": _require_dict_key(rb_prep, "preparation_id", "rb_prep"),
                    "consent_token": _require_dict_key(rb_prep, "consent_token", "rb_prep"),
                    "grants": extract_prep_grants(rb_prep),
                    "state_policy": "reset",
                    "target_config_digest": rb_digest,
                    "reauth_token": reauth_rb,
                },
                if_match=str(p2_dis["revision"]),
                idempotency_key=uniq("t4s2rb"),
            )
            assert poll_operation(ctx, _require_dict_key(rb_op, "operation_id", "rollback"))["state"] == "succeeded"
            p_final = enable_plugin(ctx, pid)
            assert p_final.get("version") == "1.0.0"
            assert int(p_final["generation"]) > int(p2["generation"])
        finally:
            _safe_cleanup_plugin(ctx, pid)

    # -----------------------------------------------------------------------
    # Scenario 3: Crash-Resilient Staged Install & Upgrade Recovery
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-03",
        tier=4,
        feature_id=5,
        name="Scenario 3: Crash-resilient staged install and upgrade recovery across SIGKILL",
        self_test=False,
        related_features=(5, 6, 8),
    )
    def test_t4_scen03_crash_resilient_install_and_upgrade(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        pin_trust_key(ctx, kp, label=uniq("t4s3"))
        pid = f"org.example.{uniq('t4s3')}"
        pkg1 = build_cmplugin(plugin_id=pid, version="1.0.0", kind="task")
        pkg2 = build_cmplugin(plugin_id=pid, version="1.1.0", kind="task")
        up1 = upload_cmplugin(ctx, pkg1.bytes, sig_text=sign_cmplugin_sha256(pkg1.bytes, kp, hashed=True))
        up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True))

        prep1_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up1["upload_id"]},
            if_match="0",
            idempotency_key=uniq("t4s3p1"),
        )
        prep1 = (poll_operation(ctx, _require_dict_key(prep1_op, "operation_id", "prepare1")).get("result")) or {}
        k_inst = uniq("t4s3kinst")
        inst_body = {
            "preparation_id": _require_dict_key(prep1, "preparation_id", "prep1"),
            "consent_token": _require_dict_key(prep1, "consent_token", "prep1"),
            "grants": extract_prep_grants(prep1),
        }
        try:
            inst_op = ctx.call_phase1(
                "POST",
                "/api/v1/plugin-installations",
                json_body=inst_body,
                if_match="0",
                idempotency_key=k_inst,
            )
            inst_op_id = str(_require_dict_key(inst_op, "operation_id", "install"))
            restart_and_relogin(ctx, kill=True)
            op_after = ctx.call_phase1("GET", f"/api/v1/plugin-operations/{inst_op_id}")
            if op_after.get("state") not in ("succeeded", "failed", "cancelled"):
                assert "revision" in op_after, f"Missing revision on operation: {op_after!r}"
                try:
                    ctx.call_phase1(
                        "POST",
                        f"/api/v1/plugin-operations/{inst_op_id}/resume",
                        json_body={},
                        if_match=str(op_after["revision"]),
                        idempotency_key=uniq("t4s3ires"),
                    )
                except ApiError as exc:
                    if exc.status != 409:
                        raise
            assert poll_operation(ctx, inst_op_id)["state"] == "succeeded"

            plugins_list = _list_all_paginated(ctx, "/api/v1/plugins", "plugins")
            assert sum(1 for p in plugins_list if isinstance(p, dict) and p.get("id") == pid) == 1
            p1 = enable_plugin(ctx, pid)
            assert p1.get("version") == "1.0.0"

            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/config")
            p1_dis = disable_plugin(ctx, pid)
            up_prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "1.1.0",
                    "state_policy": "preserve",
                    "target_config": {"schema_revision": str(cfg0["schema_revision"]), "values": {}, "secrets": {}},
                },
                if_match=str(p1_dis["revision"]),
                idempotency_key=uniq("t4s3upprep"),
            )
            up_prep = (poll_operation(ctx, _require_dict_key(up_prep_op, "operation_id", "up_prep")).get("result")) or {}
            up_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{pid}/upgrade",
                json_body={
                    "preparation_id": _require_dict_key(up_prep, "preparation_id", "up_prep"),
                    "consent_token": _require_dict_key(up_prep, "consent_token", "up_prep"),
                    "grants": extract_prep_grants(up_prep),
                    "state_policy": "preserve",
                    "target_config_digest": _extract_target_config_digest(up_prep),
                },
                if_match=str(p1_dis["revision"]),
                idempotency_key=uniq("t4s3up"),
            )
            up_op_id = str(_require_dict_key(up_op, "operation_id", "upgrade"))
            restart_and_relogin(ctx, kill=True)
            up_after = ctx.call_phase1("GET", f"/api/v1/plugin-operations/{up_op_id}")
            if up_after.get("state") not in ("succeeded", "failed", "cancelled"):
                assert "revision" in up_after, f"Missing revision on operation: {up_after!r}"
                try:
                    ctx.call_phase1(
                        "POST",
                        f"/api/v1/plugin-operations/{up_op_id}/resume",
                        json_body={},
                        if_match=str(up_after["revision"]),
                        idempotency_key=uniq("t4s3ures"),
                    )
                except ApiError as exc:
                    if exc.status != 409:
                        raise
            up_done = poll_operation(ctx, up_op_id)
            assert up_done["state"] in ("succeeded", "failed", "rejected")
            p_rec = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}")
            assert p_rec.get("version") in ("1.0.0", "1.1.0")
            assert p_rec.get("lifecycle") in ("disabled", "installed-disabled")
            enable_plugin(ctx, pid)
        finally:
            _safe_cleanup_plugin(ctx, pid)

    # -----------------------------------------------------------------------
    # Scenario 4: Multi-Provider Indexer + Hook Pipeline + Verified Download
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-04",
        tier=4,
        feature_id=9,
        name="Scenario 4: Multi-provider search (YAML + WASM) + workflow hook pipeline + verified .torrent submission",
        self_test=False,
        related_features=(9, 10, 11, 14, 15, 16, 17, 21),
    )
    def test_t4_scen04_multi_provider_hooks_and_verified_download(ctx: TestContext) -> None:
        tf = build_torrent_bytes(name="E2E Demo Movie 2026 1080p WEB-DL", length=1_048_576)
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.default_torrent = tf
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit()

        yaml_pid = f"org.example.{uniq('t4s4y')}"
        idx_pid = f"org.example.{uniq('t4s4i')}"
        hook_pid = f"org.example.{uniq('t4s4h')}"
        try:
            dl = ensure_downloader(ctx, qb.url)
            lib = ensure_library(ctx)
            setup_signed_plugin_ready(ctx, plugin_id=yaml_pid, kind="yaml-only", site_id="e2e-demo", enable=True)
            setup_signed_plugin_ready(ctx, plugin_id=idx_pid, kind="indexer", enable=True)
            setup_signed_plugin_ready(
                ctx,
                plugin_id=hook_pid,
                kind="hook",
                hooks=[
                    {"id": "search.before", "priority": 100},
                    {"id": "search.site.after", "priority": 200},
                    {"id": "search.after", "priority": 300},
                    {"id": "filter.after", "priority": 200},
                    {"id": "subscribe.score", "priority": 200},
                    {"id": "subscribe.select", "priority": 200},
                    {"id": "download.before", "priority": 200},
                    {"id": "download.after", "priority": 400},
                ],
                hook_permissions=[
                    "hooks:search.before",
                    "hooks:search.site.after",
                    "hooks:search.after",
                    "hooks:filter.after",
                    "hooks:subscribe.score",
                    "hooks:subscribe.select",
                    "hooks:download.before",
                    "hooks:download.after",
                    "events:download.after",
                    "http",
                    "kv",
                ],
                http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
                enable=True,
            )
            ensure_site(ctx, pt.url, profile_id=f"plugin:{yaml_pid}/e2e-demo")
            ensure_site(ctx, pt.url, profile_id=f"plugin:{idx_pid}/fixture")
            ensure_plugin_enabled(ctx, "builtin.boost")

            search_res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E+Demo+Movie")
            items = search_res.get("items") if isinstance(search_res, dict) else search_res
            assert isinstance(items, list) and any(
                "E2E Demo Movie 2026 1080p WEB-DL" in str(it) for it in items
            ), f"Expected movie title in multi-provider search: {search_res!r}"
            ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=site-auth-http")
            assert len(pt.request_log) >= 2

            sub = create_subscribe(ctx, lib["id"], title="E2E Demo Movie")
            ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
            assert tf.info_hash in qb.torrents, f"Expected torrent in qB: {qb.torrents!r}"
            assert qb.torrents[tf.info_hash]["size"] == tf.verified_total_bytes
            adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
            assert len(adds) >= 1 and adds[0]["detail"]["file_keys"] == ["torrents"]

            _wait_for_webhooks(pt, min_count=1)
            assert len(pt.webhook_events) >= 1
            audit = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{hook_pid}/audit?limit=100"), "hook audit")
            audit_str = str(audit)
            for hid in (
                "search.before",
                "search.site.after",
                "search.after",
                "filter.after",
                "subscribe.score",
                "subscribe.select",
                "download.before",
                "download.after",
            ):
                assert hid in audit_str, f"Expected {hid} in hook audit: {audit!r}"
            assert dl["id"]
        finally:
            _safe_cleanup_plugin(ctx, hook_pid)
            _safe_cleanup_plugin(ctx, idx_pid)
            _safe_cleanup_plugin(ctx, yaml_pid)

    # -----------------------------------------------------------------------
    # Scenario 5: Dishonest Indexer + Foreign Same-Hash + Unknown Add
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-05",
        tier=4,
        feature_id=15,
        name="Scenario 5: Dishonest 1TiB size rejected + foreign same-hash preserved + dropped add unknown reconciliation",
        self_test=False,
        related_features=(15, 16, 17),
    )
    def test_t4_scen05_dishonest_foreign_and_unknown_reconcile(ctx: TestContext) -> None:
        t_foreign = build_torrent_bytes(name="Foreign.SameHash.2026", length=8192)
        t_dishonest = build_dishonest_torrent(actual_bytes=1024 ** 4)
        t_unknown = build_torrent_bytes(name="Unknown.Reconcile.2026", length=16384)
        pt = ctx.new_pt_site()
        pt.torrent_payloads["foreign"] = t_foreign.bytes
        pt.torrent_payloads["dishonest"] = t_dishonest.bytes
        pt.torrent_payloads["unknown"] = t_unknown.bytes
        qb = ctx.new_qbit(disk_free_bytes=10 * 1024 ** 3)
        qb.seed_foreign_torrent(t_foreign.info_hash, name="Foreign.SameHash.2026", size=8192, tags="manual")

        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ensure_plugin_enabled(ctx, "builtin.boost")

        # 1. Dishonest 1 TiB vs 1B advertised
        try:
            op_dis = ctx.call_phase1(
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
                idempotency_key=uniq("t4s5dis"),
            )
            done_dis = poll_operation(ctx, _require_dict_key(op_dis, "operation_id", "dishonest add"))
            assert done_dis["state"] in ("failed", "rejected")
        except ApiError as exc:
            assert exc.status in (400, 409, 422)
        assert list(qb.torrents.keys()) == [t_foreign.info_hash]

        # 2. Foreign same-hash cannot be overwritten or deleted
        try:
            op_for = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/builtin.boost/commands",
                json_body={
                    "kind": "downloader.add",
                    "payload": {
                        "downloader_id": dl["id"],
                        "site_id": site["id"],
                        "download_url": f"{pt.url}/torrents/foreign.torrent",
                        "title": "Foreign Same Hash",
                    },
                },
                idempotency_key=uniq("t4s5for"),
            )
            done_for = poll_operation(ctx, _require_dict_key(op_for, "operation_id", "foreign add"))
            assert done_for["state"] in ("failed", "rejected", "blocked")
        except ApiError as exc:
            assert exc.status in (400, 409, 422)
        assert qb.torrents[t_foreign.info_hash]["tags"] == "manual"
        assert qb.deleted_log == []

        # 3. Dropped qB response enters unknown and reconciles across SIGKILL restart
        qb.drop_response_on_next_add = True
        try:
            res_unk = ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/torrents/unknown.torrent",
                    "title": "Unknown Reconcile",
                },
                idempotency_key=uniq("t4s5unk"),
            )
            if isinstance(res_unk, dict) and res_unk.get("operation_id"):
                d_unk = poll_operation(
                    ctx, res_unk["operation_id"], terminal_states=("unknown", "succeeded", "failed")
                )
                assert d_unk["state"] in ("unknown", "succeeded")
        except ApiError as exc:
            assert exc.status in (400, 500, 502, 504)
        qb.drop_response_on_next_add = False
        restart_and_relogin(ctx, kill=True)
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add" and t_unknown.info_hash in qb.torrents]
        assert len(adds) >= 1 and len(qb.torrents) == 2
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        assert any(t.get("info_hash") == t_unknown.info_hash for t in tasks if isinstance(t, dict))
        assert not any(t.get("info_hash") == t_foreign.info_hash for t in tasks if isinstance(t, dict))

    # -----------------------------------------------------------------------
    # Scenario 6: Site Check-in Window Deduplication & Cookie CAS
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-06",
        tier=4,
        feature_id=13,
        name="Scenario 6: Site check-in window dedup, cookie rotation CAS, and site.checkin.after outbox",
        self_test=False,
        related_features=(11, 13, 14, 21),
    )
    def test_t4_scen06_checkin_window_dedup_and_cookie_cas(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=v1")
        pt.checkin_rotate_cookie = "pass=v2"
        checkin = _find_first_party_plugin(ctx, ("builtin.checkin", "org.example.checkin"), "checkin")
        checkin_id = checkin["id"]
        ensure_plugin_enabled(ctx, checkin_id)

        hook_pid = f"org.example.{uniq('t4s6')}"
        maint_pid = f"org.example.{uniq('t4s6m')}"
        try:
            setup_signed_plugin_ready(
                ctx,
                plugin_id=hook_pid,
                kind="hook",
                hooks=[{"id": "site.checkin.after", "priority": 400}],
                hook_permissions=["hooks:site.checkin.after", "events:site.checkin.after", "http", "kv"],
                http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
                enable=True,
            )
            setup_signed_plugin_ready(ctx, plugin_id=maint_pid, kind="maintenance", enable=True)
            site = ensure_site(ctx, pt.url, profile_id="demo", cookie="uid=42; pass=v1")
            op1 = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{checkin_id}/commands",
                json_body={"kind": "site.maintenance", "payload": {"site_id": site["id"], "action": "check-in"}},
                idempotency_key=uniq("t4s6c1"),
            )
            assert poll_operation(ctx, _require_dict_key(op1, "operation_id", "checkin 1"))["state"] == "succeeded"
            assert pt.checkin_post_count == 1
            _wait_for_webhooks(pt, min_count=1)
            assert len(pt.webhook_events) == 1
            assert len(_extract_event_ids(pt.webhook_events)) == 1

            op2 = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{checkin_id}/commands",
                json_body={"kind": "site.maintenance", "payload": {"site_id": site["id"], "action": "check-in"}},
                idempotency_key=uniq("t4s6c2"),
            )
            assert poll_operation(ctx, _require_dict_key(op2, "operation_id", "checkin 2"))["state"] == "succeeded"
            assert pt.checkin_post_count == 1
            assert len(pt.webhook_events) == 1

            # Second maintenance provider re-check-in in same window deduplicates (already-completed)
            st_before_act2 = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
            act2 = ctx.call_phase1(
                "POST",
                f"/api/v1/sites/{site['id']}/plugin-actions",
                json_body={"action": "check-in", "provider_id": f"plugin:{maint_pid}/e2e-maint"},
                if_match=str(st_before_act2["revision"]),
                idempotency_key=uniq("t4s6p2"),
            )
            if isinstance(act2, dict) and act2.get("operation_id"):
                act2 = poll_operation(ctx, str(act2["operation_id"]))
                assert act2["state"] == "succeeded"
            assert pt.checkin_post_count == 1
            assert len(pt.webhook_events) == 1
            st_dedup = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
            assert (
                "already" in str(act2).lower()
                or "already" in str(st_dedup).lower()
                or "completed" in str(st_dedup).lower()
            ), f"Expected already-completed deduplication evidence, got act2={act2!r}, status={st_dedup!r}"

            st1 = st_dedup
            assert isinstance(st1, dict) and st1.get("credential_epoch"), f"Missing credential_epoch: {st1!r}"
            ctx.call_phase1("PATCH", f"/api/v1/sites/{site['id']}", json_body={"cookie": "uid=42; pass=manual_v3"})
            pt.expected_cookie = "uid=42; pass=manual_v3"
            st2 = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
            assert isinstance(st2, dict) and st2.get("credential_epoch"), f"Missing credential_epoch: {st2!r}"
            assert str(st2["credential_epoch"]) != str(st1["credential_epoch"]), (
                f"Expected credential_epoch rotation: {st1!r} -> {st2!r}"
            )

            ver = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
            assert isinstance(ver, dict) and ver.get("ok") is True
            assert "pass=manual_v3" in pt.request_log[-1]["headers"].get("cookie", "")
            assert "manual_v3" not in str(ver) and "manual_v3" not in str(st2)

            # Subsequent check-in/maintenance in the same window neither re-POSTs nor overwrites manual_v3
            op3 = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{checkin_id}/commands",
                json_body={"kind": "site.maintenance", "payload": {"site_id": site["id"], "action": "check-in"}},
                idempotency_key=uniq("t4s6c3"),
            )
            assert poll_operation(ctx, _require_dict_key(op3, "operation_id", "checkin 3"))["state"] == "succeeded"
            assert pt.checkin_post_count == 1
            ver2 = ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/verify", json_body={})
            assert isinstance(ver2, dict) and ver2.get("ok") is True
            assert "pass=manual_v3" in pt.request_log[-1]["headers"].get("cookie", "")
        finally:
            _safe_cleanup_plugin(ctx, maint_pid)
            _safe_cleanup_plugin(ctx, hook_pid)

    # -----------------------------------------------------------------------
    # Scenario 7: Narrow BrowserSession Trust Boundary
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-07",
        tier=4,
        feature_id=12,
        name="Scenario 7: Narrow BrowserSession trust boundary (built-in vs untrusted denial vs post-nav timeout)",
        self_test=False,
        related_features=(10, 12, 13),
    )
    def test_t4_scen07_narrow_browser_session_boundary(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        cdp = ctx.new_cdp()
        cdp_to = ctx.new_cdp(post_nav_timeout=True)
        untrusted_pid = f"org.example.{uniq('t4s7')}"
        try:
            put_browser_settings(ctx, True, cdp.url)
            _, _, p_untr = setup_signed_plugin_ready(ctx, plugin_id=untrusted_pid, kind="maintenance", enable=True)
            site1 = ensure_site(ctx, pt.url, profile_id="demo")
            ctx.call_phase1("PATCH", f"/api/v1/sites/{site1['id']}", json_body={"maintenance_via": "browser"})

            avail_bad = ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/{untrusted_pid}/browser-availability?site_id={site1['id']}&purpose=maintenance",
            )
            assert avail_bad.get("status") != "ready"
            assert (
                "browser-isolation-unavailable" in str(avail_bad).lower()
                or bool(avail_bad.get("missing_requirements"))
            )
            try:
                chk = ctx.call_phase1(
                    "POST",
                    f"/api/v1/plugins/{untrusted_pid}/browser-checks",
                    json_body={"site_id": site1["id"], "purpose": "maintenance"},
                    if_match=str(p_untr["revision"]),
                    idempotency_key=uniq("t4s7untr"),
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

            ensure_plugin_enabled(ctx, "builtin.checkin")
            grant_site_browser_permission(ctx, "builtin.checkin", site1["id"], "browser:maintenance")
            r_ok = ctx.call_phase1("POST", f"/api/v1/sites/{site1['id']}/check-in", json_body={})
            if isinstance(r_ok, dict) and r_ok.get("operation_id"):
                assert poll_operation(ctx, r_ok["operation_id"])["state"] == "succeeded"
            assert len(cdp.rpc_log) > 0 and len(cdp.disposed_contexts) >= 1 and len(cdp.active_contexts) == 0

            # Post-navigation timeout on a second site freezes unknown without HTTP fallback
            pt2 = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
            put_browser_settings(ctx, True, cdp_to.url)
            site2 = ensure_site(ctx, pt2.url, profile_id="pterclub")
            ctx.call_phase1("PATCH", f"/api/v1/sites/{site2['id']}", json_body={"maintenance_via": "browser"})
            grant_site_browser_permission(ctx, "builtin.checkin", site2["id"], "browser:maintenance")
            try:
                r_to = ctx.call_phase1("POST", f"/api/v1/sites/{site2['id']}/check-in", json_body={})
                if isinstance(r_to, dict) and r_to.get("operation_id"):
                    done_to = poll_operation(
                        ctx,
                        r_to["operation_id"],
                        terminal_states=("unknown", "failed", "blocked", "rejected", "succeeded"),
                    )
                    assert done_to["state"] != "succeeded"
            except ApiError as exc:
                assert exc.status in (400, 409, 502, 504)
                assert "cdp timeout" in str(exc).lower() or "unknown" in str(exc).lower() or "failed" in str(exc).lower()
            assert pt2.checkin_post_count == 0
            assert len(cdp_to.active_contexts) == 0
        finally:
            _safe_restore_browser(ctx)
            _safe_cleanup_plugin(ctx, untrusted_pid)

    # -----------------------------------------------------------------------
    # Scenario 8: SSRF & Credential Isolation Gauntlet
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-08",
        tier=4,
        feature_id=10,
        name="Scenario 8: SSRF & CredentialSlot isolation gauntlet across loopback, RFC1918, IPv6, redirects, and downgrade",
        self_test=False,
        related_features=(10, 11),
    )
    def test_t4_scen08_ssrf_and_credential_isolation_gauntlet(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        pt_a = ctx.new_pt_site(expected_cookie="uid=42; pass=secretA")
        pt_b = ctx.new_pt_site()
        spy_pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site_a = ensure_site(ctx, pt_a.url, profile_id="pterclub", cookie="uid=42; pass=secretA")

        for bad_url in (
            f"{ctx.base_url}/api/v1/plugins",
            "http://127.0.0.1:9/a.torrent",
            "http://10.0.0.1/a.torrent",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::ffff:127.0.0.1]/secret.torrent",
            f"{spy_pt.url}/spy",
        ):
            assert_api_error(
                lambda u=bad_url: ctx.call_phase1(
                    "POST",
                    "/api/v1/downloaders/submit",
                    json_body={"downloader_id": dl["id"], "download_url": u, "title": "SSRF"},
                    idempotency_key=uniq("t4s8ssrf"),
                ),
                (400, 403, 422),
                label=f"SSRF target {bad_url}",
            )
        assert len(spy_pt.spy_requests) == 0

        pt_a.downgrade_target_url = f"{pt_b.url}/spy"
        for redir_url, lbl in (
            (f"{pt_a.url}/redirect/7", ">5 hop redirect"),
            (f"{pt_a.url}/redirect/to?url={pt_b.url}/spy", "cross-origin redirect"),
            (f"{pt_a.url}/downgrade", "downgrade redirect to spy"),
        ):
            assert_api_error(
                lambda u=redir_url: ctx.call_phase1(
                    "POST",
                    "/api/v1/downloaders/submit",
                    json_body={
                        "downloader_id": dl["id"],
                        "site_id": site_a["id"],
                        "download_url": u,
                        "title": lbl,
                    },
                    idempotency_key=uniq("t4s8red"),
                ),
                (400, 403, 502),
                label=lbl,
            )
        for req in pt_b.spy_requests:
            assert "secretA" not in req["headers"].get("cookie", "")
        assert len(qb.torrents) == 0

        try:
            set_developer_mode(ctx, True)
            bad_http_pkg = build_cmplugin(
                plugin_id=f"org.example.{uniq('t4s8bad')}",
                kind="hook",
                http_rules=[{"origin": "http://169.254.169.254", "path_prefix": "/", "methods": ["GET"]}],
            )
            reauth = obtain_reauth(ctx, purpose="plugin:upload")
            assert_api_error(
                lambda: post_multipart_phase1(
                    ctx.client,
                    "/api/v1/plugin-uploads",
                    fields={"upload_id": uniq("up")},
                    files=[("file", "bad.cmplugin", bad_http_pkg.bytes, "application/zip")],
                    reauth_token=reauth,
                    idempotency_key=uniq("t4s8up"),
                    if_match="0",
                ),
                (400, 422),
                label="metadata IP in manifest [[http]]",
            )
        finally:
            _safe_restore_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Scenario 9: Real Boost Loop + 10-Min Cleanup Confirmation Flow
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-09",
        tier=4,
        feature_id=18,
        name="Scenario 9: Real boost loop (used+reserved+retained_file_bytes), HR protection, 10-min cleanup & legacy 404",
        self_test=False,
        related_features=(16, 17, 18, 26),
    )
    def test_t4_scen09_real_boost_loop_and_cleanup_confirmation(ctx: TestContext) -> None:
        save_dir = ctx.tmp_dir / "t4s9_qb"
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit(real_save_dir=save_dir, disk_free_bytes=50 * 1024 * 1024)
        fh = "e" * 40
        qb.seed_foreign_torrent(fh, name="Foreign.HR.Protect", size=4096, tags="manual")
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        boost = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        boost_id = boost["id"]
        ensure_plugin_enabled(ctx, boost_id)

        assert ctx.client.raw_request("GET", "/api/v1/sites/boost-stats").status in (404, 410)
        assert ctx.client.raw_request(
            "PATCH",
            f"/api/v1/sites/{site['id']}/boost",
            body=b"{}",
            headers={"Content-Type": "application/json"},
        ).status in (404, 410)

        add_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "T4S9 Clean Target",
                },
            },
            idempotency_key=uniq("t4s9add"),
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
            idempotency_key=uniq("t4s9del"),
        )
        assert poll_operation(ctx, _require_dict_key(del_op, "operation_id", "delete-task"))["state"] == "succeeded"
        assert list(save_dir.glob("**/*.mkv"))

        st1 = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{boost_id}/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(_require_dict_key(st1, "retained_file_bytes", "brush-state")) > 0
        assert "used_bytes" in st1 and "reserved_bytes" in st1

        run_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={"kind": "boost.run", "payload": {"site_id": site["id"], "downloader_id": dl["id"], "dry_run": False}},
            idempotency_key=uniq("t4s9run"),
        )
        assert poll_operation(ctx, _require_dict_key(run_op, "operation_id", "boost.run"))["state"] == "succeeded"
        assert fh in qb.torrents

        sel_prev = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{boost_id}/brush-cleanup-preview?task_ids={tid}",
        )
        assert sel_prev.get("mode") == "selection" and sel_prev.get("confirmation_token") is None
        assert_api_error(
            lambda: ctx.call_phase1(
                "GET",
                f"/api/v1/plugins/{boost_id}/brush-cleanup-preview?task_ids={tid}&plan_handle=p1",
            ),
            (400,),
            label="mutually exclusive task_ids and plan_handle",
        )

        prep_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={
                "kind": "downloader.prepare-cleanup",
                "payload": {"site_id": site["id"], "downloader_id": dl["id"], "task_ids": [tid]},
            },
            idempotency_key=uniq("t4s9prep"),
        )
        prep_done = poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "prepare-cleanup"))
        assert prep_done["state"] == "succeeded"
        plan_handle = (prep_done.get("result") or {}).get("plan_handle")
        assert plan_handle

        plan_prev = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{boost_id}/brush-cleanup-preview?plan_handle={plan_handle}",
        )
        assert plan_prev.get("mode") == "plan" and plan_prev.get("confirmation_token")

        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{boost_id}/commands",
                json_body={
                    "kind": "downloader.delete-files",
                    "payload": {"host_file_plan_handle": plan_handle, "revision": 1, "reason": "missing token"},
                },
                idempotency_key=uniq("t4s9notok"),
            ),
            (400, 403, 409),
            label="delete-files without confirmation_token",
        )

        df_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{boost_id}/commands",
            json_body={
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": plan_handle,
                    "revision": plan_prev.get("revision", 1),
                    "confirmation_token": plan_prev["confirmation_token"],
                    "reason": "t4s9 verified cleanup",
                },
            },
            idempotency_key=uniq("t4s9df"),
        )
        assert poll_operation(ctx, _require_dict_key(df_op, "operation_id", "delete-files"))["state"] == "succeeded"
        st2 = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{boost_id}/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(st2["retained_file_bytes"]) == 0
        assert not list(save_dir.glob("**/*.mkv"))
        assert fh in qb.torrents

    # -----------------------------------------------------------------------
    # Scenario 10: Transfer Rename Relocation + ADR-0012 Removal Mid-Flight
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-10",
        tier=4,
        feature_id=19,
        name="Scenario 10: transfer.rename relocation before LibraryWorkLease + ADR-0012 library removal mid-flight",
        self_test=False,
        related_features=(14, 19, 20, 21),
    )
    def test_t4_scen10_transfer_rename_and_adr0012_midflight(ctx: TestContext) -> None:
        lib_keep = ensure_library(ctx, kind="movie", name=uniq("t4s10keep"))
        lib_victim = ensure_library(ctx, kind="movie", name=uniq("t4s10vic"))
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib_victim['id']}/default")

        hook_pid = f"org.example.{uniq('t4s10')}"
        prev_watch: Optional[str] = None
        watch_set = False
        try:
            setup_signed_plugin_ready(
                ctx,
                plugin_id=hook_pid,
                kind="hook",
                hooks=[{"id": "transfer.rename", "priority": 200}],
                enable=True,
            )
            watch_dir = ctx.tmp_dir / "t4s10_watch"
            watch_dir.mkdir(parents=True, exist_ok=True)
            (watch_dir / "Relocate.T4S10.2026.1080p.mkv").write_bytes(b"fake-t4s10-mkv")
            prev_watch = _set_watch_intake(ctx, watch_dir)
            watch_set = True
            trigger_job(ctx, "watch_intake")
            _set_watch_intake(ctx, Path(prev_watch) if prev_watch else None)
            watch_set = False

            items = unwrap_list(
                ctx.call_phase1("GET", f"/api/v1/libraries/{lib_victim['id']}/items"),
                "victim library items",
            )
            assert len(items) >= 1
            media_id = _require_dict_key(items[0], "media_item_id", "item")
            detail = ctx.call_phase1("GET", f"/api/v1/libraries/{lib_victim['id']}/items/{media_id}")
            files_list = _require_dict_key(detail, "files", "item detail")
            assert isinstance(files_list, list) and len(files_list) >= 1, f"Expected files in {detail!r}"
            file_path = _require_dict_key(files_list[0], "path", "item detail file")
            assert str(file_path).startswith(lib_victim["root_paths"][0])
            audit = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{hook_pid}/audit?limit=50"), "audit")
            assert "transfer.rename" in str(audit)

            ctx.call_phase1(
                "POST",
                "/api/v1/playback/marks",
                json_body={"media_item_id": media_id, "favorite": True},
            )
            ctx.call_phase1("POST", f"/api/v1/libraries/{lib_victim['id']}/scan", json_body={})
            del_op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{lib_victim['id']}")
            op_id = _require_dict_key(del_op, "operation_id", "DELETE library")

            libs_now = unwrap_list(ctx.call_phase1("GET", "/api/v1/libraries"), "libraries")
            assert not any(l.get("id") == lib_victim["id"] for l in libs_now if isinstance(l, dict))
            assert any(l.get("id") == lib_keep["id"] for l in libs_now if isinstance(l, dict))
            assert_api_error(
                lambda: ctx.call_phase1("GET", f"/api/v1/libraries/{lib_victim['id']}/items"),
                (404, 409),
                label="ADR-0012 mid-flight items gate",
            )
            assert poll_operation_removal(ctx, op_id)["state"] == "succeeded"

            favs = unwrap_items(ctx.call_phase1("GET", "/api/v1/playback/favorites"), "favorites")
            up_next = unwrap_items(ctx.call_phase1("GET", "/api/v1/playback/up-next"), "up-next")
            assert not any(f.get("media_item_id") == media_id for f in favs if isinstance(f, dict))
            assert not any(u.get("media_item_id") == media_id for u in up_next if isinstance(u, dict))
            assert Path(file_path).is_file()
        finally:
            if watch_set:
                _safe_restore_watch_intake(ctx, prev_watch)
            _safe_cleanup_plugin(ctx, hook_pid)

    # -----------------------------------------------------------------------
    # Scenario 11: Author CLI End-to-End Dev Workflow
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-11",
        tier=4,
        feature_id=23,
        name="Scenario 11: Author CLI end-to-end dev workflow (scaffold -> validate -> test -> build -> pack -> sign -> dev)",
        self_test=False,
        related_features=(1, 2, 3, 22, 23, 24, 26),
    )
    def test_t4_scen11_author_cli_e2e_dev_workflow(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        proj = ctx.tmp_dir / "t4s11_proj"
        out_pkg = ctx.tmp_dir / "t4s11.cmplugin"
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        key_file = ctx.tmp_dir / "t4s11.key"
        key_file.write_text(kp.secret_seed.hex(), encoding="utf-8")
        pid = f"org.example.{uniq('t4s11')}"

        assert run_plugin_cli(
            ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", "--ui", "react", str(proj)]
        ).returncode == 0
        toml_path = proj / "plugin.toml"
        assert toml_path.is_file() and (proj / "schemas" / "config.schema.json").is_file()
        assert (proj / "ui").is_dir()
        _rewrite_toml_fields(toml_path, {"id": pid, "version": "1.0.0"})

        assert run_plugin_cli(ctx, ["validate", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["test", str(proj), "--offline"]).returncode == 0
        assert run_plugin_cli(ctx, ["build", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["pack", str(proj), "--output", str(out_pkg)]).returncode == 0
        assert out_pkg.is_file()
        assert run_plugin_cli(ctx, ["sign", str(out_pkg), "--key", str(key_file)]).returncode == 0
        sig_path = out_pkg.with_suffix(".cmplugin.minisig")
        assert sig_path.is_file()
        assert verify_minisign(
            hashlib.sha256(out_pkg.read_bytes()).digest(),
            sig_path.read_text(encoding="utf-8"),
            kp.public_key_text,
        ).valid

        _safe_restore_developer_mode(ctx, False)
        assert run_plugin_cli(ctx, ["dev", str(proj), "--instance", ctx.base_url]).returncode != 0

        try:
            set_developer_mode(ctx, True)
            assert run_plugin_cli(ctx, ["dev", str(proj), "--instance", ctx.base_url]).returncode == 0
            plugins = _list_all_paginated(ctx, "/api/v1/plugins", "plugins")
            matched = [p for p in plugins if isinstance(p, dict) and p.get("id") == pid]
            assert len(matched) == 1, f"Dev sideloaded plugin {pid} missing in {plugins!r}"
            if matched[0].get("lifecycle") != "enabled":
                enable_plugin(ctx, pid, revision=int(matched[0]["revision"]))
            desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
            assert desc.get("contract") == "1.0.0" and desc.get("react") == "19.3.0"
            assert isinstance(desc.get("surfaces"), list) and len(desc["surfaces"]) >= 1
            pkg_hash = _require_dict_key(desc, "package_hash", "UiDescriptor")
            grant_ui_trust(ctx, pid, pkg_hash)
            asset_r = ctx.client.clone_cookie_only().get_asset(desc["entry_url"])
            assert asset_r.status == 200
            assert asset_r.header("x-content-type-options") == "nosniff"
        finally:
            _safe_cleanup_plugin(ctx, pid)
            _safe_restore_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Scenario 12: First-Party Check-in & Boost UIs + Jellyfin Separation
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-12",
        tier=4,
        feature_id=25,
        name="Scenario 12: First-party Check-in & Boost UIs (/plugin-apps), Jellyfin /plugins 404, cookie/trust revoke & upgrade",
        self_test=False,
        related_features=(22, 24, 25, 26),
    )
    def test_t4_scen12_first_party_uis_jellyfin_and_trust_lifecycle(ctx: TestContext) -> None:
        checkin = _find_first_party_plugin(ctx, ("builtin.checkin", "org.example.checkin"), "checkin")
        boost = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        checkin_id = checkin["id"]
        boost_id = boost["id"]
        ensure_plugin_enabled(ctx, checkin_id)
        ensure_plugin_enabled(ctx, boost_id)

        jf = ctx.client.raw_request("GET", f"/plugins/{checkin_id}/checkin", no_bearer=True, no_cookie=True)
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
                    "GET", f"/plugin-apps/{checkin_id}/checkin", no_bearer=True, no_cookie=True
                )
                assert app_r.status == 200
                html_txt = app_r.body.decode("utf-8", errors="ignore").lower()
                assert '<div id="root"' in html_txt or "<div id='root'" in html_txt or "<script" in html_txt
        else:
            app_r = ctx.client.raw_request(
                "GET", f"/plugin-apps/{checkin_id}/checkin", no_bearer=True, no_cookie=True
            )
            assert app_r.status == 200
            html_txt = app_r.body.decode("utf-8", errors="ignore").lower()
            assert '<div id="root"' in html_txt or "<div id='root'" in html_txt or "<script" in html_txt

        me = ctx.client.me()
        cookie_cli = ctx.client.clone_cookie_only()
        _, mc = create_member_client(ctx)

        with ctx.client.clone(token=ctx.client.token, session_cookie=None) as bearer_only:
            for fp_id in (checkin_id, boost_id):
                desc = cookie_cli.get(f"/api/v1/plugins/{fp_id}/ui")
                assert desc.get("subject_user_id") == me.get("id")
                assert desc.get("contract") == "1.0.0" and desc.get("react") == "19.3.0"
                kinds = {s["kind"] for s in _require_dict_key(desc, "surfaces", f"{fp_id} ui")}
                assert {"config", "page", "app", "dashboard"}.issubset(kinds)
                grant_ui_trust(ctx, fp_id, _require_dict_key(desc, "package_hash", f"{fp_id} ui"))
                asset_ok = cookie_cli.get_asset(desc["entry_url"], use_cookie=True, no_bearer=True)
                assert asset_ok.status == 200 and asset_ok.header("x-content-type-options") == "nosniff"
                assert "no-store" in (asset_ok.header("cache-control") or "").lower()
                assert ctx.client.raw_request("GET", desc["entry_url"], no_bearer=True, no_cookie=True).status == 401
                assert bearer_only.raw_request("GET", desc["entry_url"], no_cookie=True).status == 401
                assert mc.get_asset(desc["entry_url"], use_cookie=True, no_bearer=True).status == 403

        nav = unwrap_list(cookie_cli.get("/api/v1/plugin-ui-navigation"), "navigation")
        assert any(n.get("plugin_id") == checkin_id for n in nav if isinstance(n, dict))
        assert_api_error(lambda: mc.get("/api/v1/plugin-ui-navigation"), (403,), label="member navigation")

        # Trust revoke + upgrade invalidation on custom ui-only plugin
        custom_pid = f"org.example.{uniq('t4s12')}"
        try:
            pkg1, kp, _ = setup_signed_plugin_ready(
                ctx, plugin_id=custom_pid, version="1.0.0", kind="ui-only", enable=True
            )
            granted = grant_ui_trust(ctx, custom_pid, pkg1.sha256_hex)
            desc_v1 = cookie_cli.get(f"/api/v1/plugins/{custom_pid}/ui")
            assert cookie_cli.get_asset(desc_v1["entry_url"]).status == 200

            ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{custom_pid}/ui-trust/revoke",
                json_body={"package_hash": pkg1.sha256_hex},
                if_match=str(granted.get("revision", 1)),
                idempotency_key=uniq("t4s12rev"),
            )
            assert cookie_cli.get_asset(desc_v1["entry_url"]).status == 403
            assert ctx.call_phase1("GET", f"/api/v1/plugins/{custom_pid}").get("lifecycle") == "enabled"

            grant_ui_trust(ctx, custom_pid, pkg1.sha256_hex)
            pkg2 = build_cmplugin(
                plugin_id=custom_pid,
                version="1.1.0",
                kind="ui-only",
                extra_ui_assets={"ui/assets/v2.js": (b"export const v = 2;\n", "application/javascript")},
            )
            up2 = upload_cmplugin(ctx, pkg2.bytes, sig_text=sign_cmplugin_sha256(pkg2.bytes, kp, hashed=True))
            cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{custom_pid}/config")
            p_dis = disable_plugin(ctx, custom_pid)
            prep_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{custom_pid}/upgrades/prepare",
                json_body={
                    "upload_id": up2["upload_id"],
                    "version": "1.1.0",
                    "state_policy": "preserve",
                    "target_config": {"schema_revision": str(cfg0["schema_revision"]), "values": {}, "secrets": {}},
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t4s12prep"),
            )
            prep = (poll_operation(ctx, _require_dict_key(prep_op, "operation_id", "upgrades/prepare")).get("result")) or {}
            up_op = ctx.call_phase1(
                "POST",
                f"/api/v1/plugins/{custom_pid}/upgrade",
                json_body={
                    "preparation_id": prep["preparation_id"],
                    "consent_token": prep["consent_token"],
                    "grants": extract_prep_grants(prep),
                    "state_policy": "preserve",
                    "target_config_digest": _extract_target_config_digest(prep),
                },
                if_match=str(p_dis["revision"]),
                idempotency_key=uniq("t4s12up"),
            )
            assert poll_operation(ctx, _require_dict_key(up_op, "operation_id", "upgrade"))["state"] == "succeeded"
            enable_plugin(ctx, custom_pid)
            desc_v2 = cookie_cli.get(f"/api/v1/plugins/{custom_pid}/ui")
            assert desc_v2.get("package_hash") == pkg2.sha256_hex
            assert (desc_v2.get("trust") or {}).get("status") != "trusted"
            assert cookie_cli.get_asset(desc_v2["entry_url"]).status == 403
        finally:
            _safe_cleanup_plugin(ctx, custom_pid)

    # -----------------------------------------------------------------------
    # Scenario 13: Transactional Outbox Delivery & Event Crash Recovery
    # -----------------------------------------------------------------------

    @registry.case(
        id="T4-SCEN-13",
        tier=4,
        feature_id=21,
        name="Scenario 13: Transactional outbox delivery (rollback=0 events; committed events survive SIGKILL & dedup)",
        self_test=False,
        related_features=(14, 21),
    )
    def test_t4_scen13_transactional_outbox_crash_recovery_dedup(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit()
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        dl = ensure_downloader(ctx, qb.url)
        site1 = ensure_site(ctx, pt.url, profile_id="pterclub")
        site2 = ensure_site(ctx, pt.url, profile_id="pterclub")
        obs_pid = f"org.example.{uniq('t4s13')}"
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
                        "title": "T4S13 Rollback",
                    },
                    idempotency_key=uniq("t4s13rb"),
                ),
                (400, 403, 422),
                label="rollback emits 0 outbox events",
            )
            assert len(pt.webhook_events) == 0

            # 2. Committed events across app.db, subscribe.db, library.db
            ctx.call_phase1("POST", f"/api/v1/sites/{site1['id']}/check-in", json_body={})
            _wait_for_webhooks(pt, min_count=1)

            ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "T4S13 Download",
                },
                idempotency_key=uniq("t4s13dl"),
            )
            _wait_for_webhooks(pt, min_count=2)

            watch_dir = ctx.tmp_dir / "t4s13_watch"
            watch_dir.mkdir(parents=True, exist_ok=True)
            (watch_dir / "Outbox.T4S13.2026.1080p.mkv").write_bytes(b"fake-t4s13-mkv")
            prev_watch = _set_watch_intake(ctx, watch_dir)
            watch_set = True
            trigger_job(ctx, "watch_intake")
            _set_watch_intake(ctx, Path(prev_watch) if prev_watch else None)
            watch_set = False
            _wait_for_webhooks(pt, min_count=3)

            ev_ids_before = _extract_event_ids(pt.webhook_events)
            assert len(ev_ids_before) == 3, f"Expected 3 unique event_ids, got {ev_ids_before!r}"
            assert len(set(ev_ids_before)) == 3

            # 3. Crash & replay recovery with transient webhook failure
            pt.webhook_fail_next = 1
            chk_res = ctx.call_phase1("POST", f"/api/v1/sites/{site2['id']}/check-in", json_body={})
            if isinstance(chk_res, dict) and chk_res.get("operation_id"):
                assert poll_operation(ctx, str(chk_res["operation_id"]))["state"] == "succeeded"
            restart_and_relogin(ctx, kill=True)
            _wait_for_webhooks(pt, min_count=4)
            time.sleep(0.2)
            ev_ids_after = _extract_event_ids(pt.webhook_events)
            assert len(ev_ids_after) == 4, f"Expected 4 total deliveries after crash retry, got {ev_ids_after!r}"
            assert len(set(ev_ids_after)) == 4, f"Duplicate event_id delivered across restart: {ev_ids_after!r}"
        finally:
            if watch_set:
                _safe_restore_watch_intake(ctx, prev_watch)
            _safe_cleanup_plugin(ctx, obs_pid)
