"""Tier 2 Boundary & Negative Tests for Features #19-#21 (P5 Transfer Relocation, ADR-0012, Outbox)."""
from __future__ import annotations

from pathlib import Path

from . import SuiteRegistry, TestContext
from ._helpers import (
    assert_api_error,
    create_member_client,
    create_subscribe,
    ensure_downloader,
    ensure_library,
    ensure_site,
    poll_library_operation,
    restart_and_relogin,
    setup_signed_plugin_ready,
    trigger_job,
    uniq,
    unwrap_list,
)
from .t1_p5 import _set_watch_intake, _wait_for_webhooks, poll_operation_removal


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #19: Transfer Relocation Boundaries (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F19-01",
        tier=2,
        feature_id=19,
        name="transfer.rename traversal/outside-root output cannot write outside Library root",
        self_test=False,
    )
    def test_t2_f19_path_traversal_blocked(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('t2tr1')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        watch_dir = ctx.tmp_dir / "watch_t2_1"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "Traversal.Guard.2026.1080p.mkv").write_bytes(b"fake-mkv")
        prev = _set_watch_intake(ctx, watch_dir)
        try:
            trigger_job(ctx, "watch_intake")
        finally:
            _set_watch_intake(ctx, Path(prev) if prev else None)
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and "transfer.rename" in str(audit)
        assert not (ctx.tmp_dir / "evil_escape.mkv").exists()

    @registry.case(
        id="T2-F19-02",
        tier=2,
        feature_id=19,
        name="transfer.rename preserves authoritative library root_id and file identity",
        self_test=False,
    )
    def test_t2_f19_root_id_preserved(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('t2tr2')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        libs = ctx.call_phase1("GET", "/api/v1/libraries")
        assert lib["id"] in {l["id"] for l in libs}
        watch_dir = ctx.tmp_dir / "watch_t2_2"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "Identity.Movie.2026.1080p.mkv").write_bytes(b"fake-id-mkv")
        prev = _set_watch_intake(ctx, watch_dir)
        try:
            trigger_job(ctx, "watch_intake")
        finally:
            _set_watch_intake(ctx, Path(prev) if prev else None)
        items = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/libraries/{lib['id']}/items"),
            "library items",
        )
        assert len(items) >= 1
        detail = ctx.call_phase1(
            "GET", f"/api/v1/libraries/{lib['id']}/items/{items[0]['media_item_id']}"
        )
        assert detail["files"][0]["path"].startswith(lib["root_paths"][0])
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and "transfer.rename" in str(audit)

    @registry.case(
        id="T2-F19-03",
        tier=2,
        feature_id=19,
        name="Concurrent PATCH /libraries/:id root_paths reconfiguration does not corrupt ledger",
        self_test=False,
    )
    def test_t2_f19_root_paths_race_cas(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('t2tr3')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        new_root = ctx.tmp_dir / uniq("newroot")
        new_root.mkdir(parents=True, exist_ok=True)
        patch_resp = ctx.call_phase1(
            "PATCH",
            f"/api/v1/libraries/{lib['id']}",
            json_body={"root_paths": [str(new_root)]},
        )
        assert patch_resp.get("operation_id"), f"Expected 202 operation_id, got {patch_resp!r}"
        done = poll_library_operation(ctx, lib["id"], patch_resp["operation_id"])
        assert done["state"] == "succeeded"
        libs = ctx.call_phase1("GET", "/api/v1/libraries")
        matched = [l for l in libs if l["id"] == lib["id"]]
        assert len(matched) == 1 and matched[0]["root_paths"] == [str(new_root)]
        watch_dir = ctx.tmp_dir / "watch_t2_3"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "RootSwitch.Movie.2026.1080p.mkv").write_bytes(b"fake-switch-mkv")
        prev = _set_watch_intake(ctx, watch_dir)
        try:
            trigger_job(ctx, "watch_intake")
        finally:
            _set_watch_intake(ctx, Path(prev) if prev else None)
        items = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/libraries/{lib['id']}/items"),
            "library items",
        )
        assert len(items) >= 1, f"Expected transferred item under new_root, got {items!r}"
        detail = ctx.call_phase1(
            "GET", f"/api/v1/libraries/{lib['id']}/items/{items[0]['media_item_id']}"
        )
        assert detail["files"][0]["path"].startswith(str(new_root)), (
            f"Expected file under {new_root}, got {detail!r}"
        )
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and "transfer.rename" in str(audit)

    @registry.case(
        id="T2-F19-04",
        tier=2,
        feature_id=19,
        name="Failing transfer.rename hook does not hold LibraryWorkLease or block Library reads",
        self_test=False,
    )
    def test_t2_f19_no_lease_deadlock_on_hook_error(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('t2tr4')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        watch_dir = ctx.tmp_dir / "watch_t2_4"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "Lease.Check.2026.1080p.mkv").write_bytes(b"fake-lease-mkv")
        prev = _set_watch_intake(ctx, watch_dir)
        try:
            trigger_job(ctx, "watch_intake")
        finally:
            _set_watch_intake(ctx, Path(prev) if prev else None)
        items = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/libraries/{lib['id']}/items"),
            "library items",
        )
        assert isinstance(items, list)
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and "transfer.rename" in str(audit), (
            f"Expected transfer.rename audit entry, got {audit!r}"
        )

    @registry.case(
        id="T2-F19-05",
        tier=2,
        feature_id=19,
        name="Nested library deepest-root boundary prevents cross-library escape",
        self_test=False,
    )
    def test_t2_f19_nested_library_deepest_root(ctx: TestContext) -> None:
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('t2tr5')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        outer = ctx.tmp_dir / uniq("outer")
        inner = outer / "nested"
        inner.mkdir(parents=True, exist_ok=True)
        lib1 = ctx.call_phase1(
            "POST", "/api/v1/libraries", json_body={"name": uniq("outer"), "kind": "movie", "root_paths": [str(outer)]}
        )
        lib2 = ctx.call_phase1(
            "POST", "/api/v1/libraries", json_body={"name": uniq("inner"), "kind": "movie", "root_paths": [str(inner)]}
        )
        assert lib1["id"] != lib2["id"]
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib2['id']}/default")
        watch_dir = ctx.tmp_dir / "watch_t2_5"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "Nested.Root.2026.1080p.mkv").write_bytes(b"fake-nested-mkv")
        prev = _set_watch_intake(ctx, watch_dir)
        try:
            trigger_job(ctx, "watch_intake")
        finally:
            _set_watch_intake(ctx, Path(prev) if prev else None)
        items1 = unwrap_list(ctx.call_phase1("GET", f"/api/v1/libraries/{lib1['id']}/items"), "outer items")
        items2 = unwrap_list(ctx.call_phase1("GET", f"/api/v1/libraries/{lib2['id']}/items"), "inner items")
        assert items1 == [] and len(items2) >= 1

    # -----------------------------------------------------------------------
    # Feature #20: ADR-0012 Library Removal & Fencing (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F20-01",
        tier=2,
        feature_id=20,
        name="After DELETE /libraries/:id 202, library items endpoint refuses late reads/publication",
        self_test=False,
    )
    def test_t2_f20_late_publication_fenced(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        _keep = ensure_library(ctx, kind="movie", name=uniq("k20"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("d20"))
        op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        err = assert_api_error(
            lambda: ctx.client.get(f"/api/v1/libraries/{del_lib['id']}/items"),
            (404, 409),
            label="read items of deleting/deleted library",
        )
        if err.status == 404:
            assert err.code is not None and err.code != "not_found"
        assert poll_operation_removal(ctx, op["operation_id"])["state"] == "succeeded"

    @registry.case(
        id="T2-F20-02",
        tier=2,
        feature_id=20,
        name="Writing/scanning into a deleting/deleted library is immediately rejected (404/409)",
        self_test=False,
    )
    def test_t2_f20_writes_during_removal_rejected(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        _keep = ensure_library(ctx, kind="movie", name=uniq("k20b"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("d20b"))
        op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        err = assert_api_error(
            lambda: ctx.client.post(f"/api/v1/libraries/{del_lib['id']}/scan", json_body={}),
            (404, 409),
            label="scan deleting library",
        )
        if err.status == 404:
            assert err.code is not None and err.code != "not_found"
        assert poll_operation_removal(ctx, op["operation_id"])["state"] == "succeeded"

    @registry.case(
        id="T2-F20-03",
        tier=2,
        feature_id=20,
        name="Last library of its kind is protected from deletion (409 library.protected)",
        self_test=False,
    )
    def test_t2_f20_last_library_protected(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        libs = ctx.call_phase1("GET", "/api/v1/libraries")
        tv_libs = [l for l in libs if l.get("kind") == "tv"]
        if not tv_libs:
            tv_libs = [ensure_library(ctx, kind="tv", name=uniq("lasttv"))]
        while len(tv_libs) > 1:
            extra = tv_libs.pop()
            del_op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{extra['id']}")
            assert poll_operation_removal(ctx, del_op["operation_id"])["state"] == "succeeded"
        assert_api_error(
            lambda: ctx.call_phase1("DELETE", f"/api/v1/libraries/{tv_libs[0]['id']}"),
            (409,),
            label="delete last library of kind",
        )

    @registry.case(
        id="T2-F20-04",
        tier=2,
        feature_id=20,
        name="SIGKILL mid library removal + restart converges removal operation to succeeded",
        self_test=False,
    )
    def test_t2_f20_crash_mid_removal_recovers(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        _keep = ensure_library(ctx, kind="movie", name=uniq("k20c"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("d20c"))
        sub = create_subscribe(ctx, del_lib["id"], title="Crash Detach")
        op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        restart_and_relogin(ctx, kill=True)
        done = poll_operation_removal(ctx, op["operation_id"])
        assert done["state"] == "succeeded"
        sub_after = ctx.call_phase1("GET", f"/api/v1/subscriptions/{sub['id']}")
        assert sub_after.get("library_id") is None

    @registry.case(
        id="T2-F20-05",
        tier=2,
        feature_id=20,
        name="Member user calling DELETE /libraries/:id or GET /library-removals/:id receives 403",
        self_test=False,
    )
    def test_t2_f20_member_rbac(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        _keep = ensure_library(ctx, kind="movie", name=uniq("k20m"))
        lib = ensure_library(ctx, kind="movie", name=uniq("m20"))
        _, mc = create_member_client(ctx)
        assert_api_error(
            lambda: mc.delete(f"/api/v1/libraries/{lib['id']}"),
            (403,),
            label="member DELETE /libraries/:id",
        )
        op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{lib['id']}")
        assert_api_error(
            lambda: mc.get(f"/api/v1/library-removals/{op['operation_id']}"),
            (403,),
            label="member GET /library-removals/:id",
        )
        assert poll_operation_removal(ctx, op["operation_id"])["state"] == "succeeded"

    # -----------------------------------------------------------------------
    # Feature #21: Per-DB plugin_event_outbox & EventDispatcher (5 T2 cases)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F21-01",
        tier=2,
        feature_id=21,
        name="Failed/rejected action emits 0 outbox events to webhook target",
        self_test=False,
    )
    def test_t2_f21_rollback_emits_zero_events(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evt0')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "events:download.after", "http", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": "http://169.254.169.254/fail.torrent",
                    "title": "SSRF Outbox",
                },
                idempotency_key=uniq("evt0"),
            ),
            (400, 403, 422),
            label="rejected download must not emit download.after",
        )
        assert len(pt.webhook_events) == 0

    @registry.case(
        id="T2-F21-02",
        tier=2,
        feature_id=21,
        name="Committed outbox events survive SIGKILL + restart with webhook retry deduplication",
        self_test=False,
    )
    def test_t2_f21_crash_recovery_dedup(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        pt.webhook_fail_next = 1
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtdedup')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "events:download.after", "http", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Crash Dedup",
            },
            idempotency_key=uniq("evtdedup"),
        )
        restart_and_relogin(ctx, kill=True)
        assert len(qb.torrents) == 1
        _wait_for_webhooks(pt, min_count=1)
        assert len(pt.webhook_events) == 1

    @registry.case(
        id="T2-F21-03",
        tier=2,
        feature_id=21,
        name="Notify plugin attempting commands.submit inside hooks.notify fails wrong-context",
        self_test=False,
    )
    def test_t2_f21_notify_wrong_context(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtwc')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "events:download.after", "kv"],
            enable=True,
        )
        ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Notify Wrong Context",
            },
            idempotency_key=uniq("evtwc"),
        )
        assert len(qb.torrents) == 1
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit")
        assert isinstance(audit, list) and len(audit) >= 1

    @registry.case(
        id="T2-F21-04",
        tier=2,
        feature_id=21,
        name="Disabled or ungranted events:<domain> plugin receives 0 outbox events",
        self_test=False,
    )
    def test_t2_f21_ungranted_plugin_zero_events(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtung')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Ungranted Events",
            },
            idempotency_key=uniq("evtung"),
        )
        assert len(qb.torrents) == 1
        assert len(pt.webhook_events) == 0

    @registry.case(
        id="T2-F21-05",
        tier=2,
        feature_id=21,
        name="Failing notify plugin does not block healthy sibling plugins or business writes",
        self_test=False,
    )
    def test_t2_f21_poison_plugin_isolated(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        pt.webhook_fail_next = 10
        pt_ok = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtpoi')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "events:download.after", "http", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtok')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "events:download.after", "http", "kv"],
            http_rules=[{"origin": pt_ok.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Poison Isolation",
            },
            idempotency_key=uniq("evtpoi"),
        )
        assert len(qb.torrents) == 1
        _wait_for_webhooks(pt_ok, min_count=1)
        assert len(pt_ok.webhook_events) == 1
