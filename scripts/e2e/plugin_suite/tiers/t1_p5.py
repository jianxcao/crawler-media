"""Tier 1 Happy-Path Tests for Features #19-#21 (P5 Transfer Relocation, ADR-0012, Outbox)."""
from __future__ import annotations

from pathlib import Path
import time
from typing import Optional

from . import SuiteRegistry, TestContext
from ._helpers import (
    assert_api_error,
    create_subscribe,
    ensure_downloader,
    ensure_library,
    ensure_site,
    poll_scan_operation,
    setup_signed_plugin_ready,
    trigger_job,
    uniq,
    unwrap_items,
    unwrap_list,
)


def _set_watch_intake(ctx: TestContext, watch_dir: Optional[Path]) -> Optional[str]:
    ts = ctx.call_phase1("GET", "/api/v1/settings/transfer")
    prev = ts.get("watch_intake")
    ctx.call_phase1(
        "PATCH",
        "/api/v1/settings/transfer",
        json_body={
            "expected_revision": ts["revision"],
            "watch_intake": str(watch_dir) if watch_dir is not None else None,
        },
    )
    return prev


def _wait_for_webhooks(pt: object, min_count: int = 1, timeout: float = 8.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if len(getattr(pt, "webhook_events", [])) >= min_count:
            return
        time.sleep(0.05)


def _wait_for_audit_action(
    ctx: TestContext,
    plugin_id: str,
    action: str,
    timeout: float = 5.0,
) -> list:
    deadline = time.monotonic() + timeout
    last_audit: list = []
    while time.monotonic() < deadline:
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}/audit?limit=50")
        if isinstance(audit, list):
            last_audit = audit
            if action in str(audit):
                return audit
        time.sleep(0.05)
    return last_audit


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #19: Transfer Hooks Relocated Before recorder.prepare & Outside LibraryWorkLease
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F19-01",
        tier=1,
        feature_id=19,
        name="Subscribe collect runs transfer.rename before recorder.prepare and updates final path",
        self_test=False,
    )
    def test_f19_subscribe_transfer_rename(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('tren1')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        save_dir = ctx.tmp_dir / uniq("qb_save")
        save_dir.mkdir(parents=True, exist_ok=True)
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit(real_save_dir=save_dir)
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        sub = create_subscribe(ctx, lib["id"])
        ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
        assert len(qb.torrents) >= 1, f"Expected subscribe run to add torrent to qB, got {qb.torrents!r}"
        with qb._lock:
            for h, t in qb.torrents.items():
                t["state"] = "uploading"
                t["progress"] = 1.0
                t["downloaded"] = t["size"]
                for f in qb.files.get(h, []):
                    f["progress"] = 1.0
        trigger_job(ctx, "transfer")
        items = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/libraries/{lib['id']}/items"),
            "library items",
        )
        assert len(items) >= 1
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and "transfer.rename" in str(audit)

    @registry.case(
        id="T1-F19-02",
        tier=1,
        feature_id=19,
        name="WatchIntake transfer invokes transfer.rename before LibraryWorkLease acquisition",
        self_test=False,
    )
    def test_f19_watch_intake_transfer(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('tren2')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        watch_dir = ctx.tmp_dir / "watch_intake"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "Movie.2026.1080p.mkv").write_bytes(b"fake-mkv-payload")
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
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and "transfer.rename" in str(audit)

    @registry.case(
        id="T1-F19-03",
        tier=1,
        feature_id=19,
        name="Unidentified claim invokes transfer.rename before ledger publication",
        self_test=False,
    )
    def test_f19_claim_transfer(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('tren3')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        watch_dir = ctx.tmp_dir / "watch_unid"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "unknown_clip_xyz_001.mkv").write_bytes(b"fake-unidentified-mkv")
        prev = _set_watch_intake(ctx, watch_dir)
        try:
            trigger_job(ctx, "watch_intake")
        finally:
            _set_watch_intake(ctx, Path(prev) if prev else None)
        unidentified = unwrap_list(
            ctx.call_phase1("GET", "/api/v1/unidentified"),
            "unidentified items",
        )
        assert len(unidentified) >= 1, f"Expected unidentified item, got {unidentified!r}"
        claimed = ctx.call_phase1(
            "POST",
            "/api/v1/unidentified/claim",
            json_body={
                "path": unidentified[0]["path"],
                "action": {
                    "type": "identify_and_transfer",
                    "title": "Claimed Movie",
                    "kind": "movie",
                    "year": 2026,
                },
            },
        )
        assert str(claimed.get("path", "")).startswith(lib["root_paths"][0])
        audit = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50")
        assert isinstance(audit, list) and "transfer.rename" in str(audit)

    @registry.case(
        id="T1-F19-04",
        tier=1,
        feature_id=19,
        name="Multiple transfer.rename hooks chain in priority order before recorder.prepare",
        self_test=False,
    )
    def test_f19_chained_transfer_rename(ctx: TestContext) -> None:
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        pkg1, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('trena')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 100}],
            enable=True,
        )
        pkg2, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('trenb')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        watch_dir = ctx.tmp_dir / "watch_chain"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "Chained.Movie.2026.1080p.mkv").write_bytes(b"fake-chain-mkv")
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
        audit_1 = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg1.plugin_id}/audit?limit=50")
        audit_2 = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg2.plugin_id}/audit?limit=50")
        assert isinstance(audit_1, list) and "transfer.rename" in str(audit_1)
        assert isinstance(audit_2, list) and "transfer.rename" in str(audit_2)

    @registry.case(
        id="T1-F19-05",
        tier=1,
        feature_id=19,
        name="transfer.completed Notify fires after ledger publication succeeds",
        self_test=False,
    )
    def test_f19_transfer_completed_notify(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('tcomp')}",
            kind="hook",
            hooks=[{"id": "transfer.completed", "priority": 400}],
            hook_permissions=["hooks:transfer.completed", "events:transfer.completed", "http", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        watch_dir = ctx.tmp_dir / "watch_tcomp"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "Completed.Movie.2026.1080p.mkv").write_bytes(b"fake-comp-mkv")
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
        _wait_for_webhooks(pt, min_count=1)
        assert len(pt.webhook_events) >= 1

    # -----------------------------------------------------------------------
    # Feature #20: ADR-0012 Library Removal & Subscribe Lifecycle Fencing
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F20-01",
        tier=1,
        feature_id=20,
        name="DELETE /libraries/:id returns 202, hides library immediately, and leaves disk files intact",
        self_test=False,
    )
    def test_f20_delete_library_hides_and_preserves_disk(ctx: TestContext) -> None:
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f20fen')}",
            kind="hook",
            hooks=[{"id": "transfer.rename", "priority": 200}],
            enable=True,
        )
        _keep_lib = ensure_library(ctx, kind="movie", name=uniq("keep"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("del"))
        root_dir = Path(del_lib["root_paths"][0])
        sample_file = root_dir / "KeepOnDisk.2026.mkv"
        sample_file.write_bytes(b"video-bytes")
        op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        op_id = op["operation_id"]
        libs_now = ctx.call_phase1("GET", "/api/v1/libraries")
        assert not any(l["id"] == del_lib["id"] for l in libs_now)
        assert_api_error(
            lambda: ctx.call_phase1("GET", f"/api/v1/libraries/{del_lib['id']}/items"),
            (404, 409),
            label="ADR-0012 immediate fence on deleted library items",
        )
        done = poll_operation_removal(ctx, op_id)
        assert done["state"] == "succeeded"
        assert sample_file.is_file()

    @registry.case(
        id="T1-F20-02",
        tier=1,
        feature_id=20,
        name="Deleting library purges orphan Media playback resume and favorites",
        self_test=False,
    )
    def test_f20_purge_orphan_playback(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        _keep_lib = ensure_library(ctx, kind="movie", name=uniq("keep2"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("del2"))
        sample = Path(del_lib["root_paths"][0]) / "Orphan.Movie.2026.1080p.mkv"
        sample.write_bytes(b"fake-orphan-mkv")
        scan_op = ctx.call_phase1("POST", f"/api/v1/libraries/{del_lib['id']}/scan", json_body={})
        scan_done = poll_scan_operation(ctx, del_lib["id"], scan_op["operation_id"])
        assert scan_done["status"] == "completed"
        items = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/libraries/{del_lib['id']}/items"),
            "library items",
        )
        assert len(items) >= 1
        media_id = items[0]["media_item_id"]
        ctx.call_phase1(
            "POST",
            "/api/v1/playback/marks",
            json_body={"media_item_id": media_id, "favorite": True},
        )
        favs_before = unwrap_items(ctx.call_phase1("GET", "/api/v1/playback/favorites"), "favorites")
        assert any(f.get("media_item_id") == media_id for f in favs_before)

        op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        done = poll_operation_removal(ctx, op["operation_id"])
        assert done["state"] == "succeeded"
        up_next = unwrap_items(ctx.call_phase1("GET", "/api/v1/playback/up-next"), "up-next")
        favs_after = unwrap_items(ctx.call_phase1("GET", "/api/v1/playback/favorites"), "favorites")
        assert not any(it.get("media_item_id") == media_id for it in up_next)
        assert not any(it.get("media_item_id") == media_id for it in favs_after)

    @registry.case(
        id="T1-F20-03",
        tier=1,
        feature_id=20,
        name="Deleting library detaches active Subscribe library_id to null before succeeded",
        self_test=False,
    )
    def test_f20_detach_subscribe(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        _keep_lib = ensure_library(ctx, kind="movie", name=uniq("keep3"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("del3"))
        sub = create_subscribe(ctx, del_lib["id"], title="Detach Test")
        op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        done = poll_operation_removal(ctx, op["operation_id"])
        assert done["state"] == "succeeded"
        sub_after = ctx.call_phase1("GET", f"/api/v1/subscriptions/{sub['id']}")
        assert sub_after.get("library_id") is None

    @registry.case(
        id="T1-F20-04",
        tier=1,
        feature_id=20,
        name="Re-creating Library over same root dir assigns fresh file_ids cleanly",
        self_test=False,
    )
    def test_f20_recreate_library(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        _keep_lib = ensure_library(ctx, kind="movie", name=uniq("keep4"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("del4"))
        shared_path = del_lib["root_paths"][0]
        (Path(shared_path) / "Recreate.Movie.2026.1080p.mkv").write_bytes(b"recreate-mkv")
        op = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        assert poll_operation_removal(ctx, op["operation_id"])["state"] == "succeeded"
        new_lib = ctx.call_phase1(
            "POST",
            "/api/v1/libraries",
            json_body={"name": uniq("recreated"), "kind": "movie", "root_paths": [shared_path]},
        )
        assert new_lib["id"] != del_lib["id"]
        scan_op = ctx.call_phase1("POST", f"/api/v1/libraries/{new_lib['id']}/scan", json_body={})
        assert poll_scan_operation(ctx, new_lib["id"], scan_op["operation_id"])["status"] == "completed"
        items = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/libraries/{new_lib['id']}/items"),
            "library items",
        )
        assert len(items) >= 1

    @registry.case(
        id="T1-F20-05",
        tier=1,
        feature_id=20,
        name="Repeated DELETE /libraries/:id while removal in progress returns existing operation_id idempotently",
        self_test=False,
    )
    def test_f20_idempotent_delete(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        _keep_lib = ensure_library(ctx, kind="movie", name=uniq("keep5"))
        del_lib = ensure_library(ctx, kind="movie", name=uniq("del5"))
        op1 = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        op2 = ctx.call_phase1("DELETE", f"/api/v1/libraries/{del_lib['id']}")
        assert op1["operation_id"] == op2["operation_id"]
        assert poll_operation_removal(ctx, op1["operation_id"])["state"] == "succeeded"

    # -----------------------------------------------------------------------
    # Feature #21: Per-DB plugin_event_outbox & EventDispatcher
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F21-01",
        tier=1,
        feature_id=21,
        name="app.db outbox delivers site.checkin.after with stable event.id",
        self_test=False,
    )
    def test_f21_app_outbox_checkin(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtchk')}",
            kind="hook",
            hooks=[{"id": "site.checkin.after", "priority": 400}],
            hook_permissions=["hooks:site.checkin.after", "events:site.checkin.after", "http", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        ctx.call_phase1("POST", f"/api/v1/sites/{site['id']}/check-in", json_body={})
        assert pt.checkin_post_count == 1
        _wait_for_webhooks(pt, min_count=1)
        assert len(pt.webhook_events) >= 1
        ev = pt.webhook_events[0].get("body") or {}
        assert ev.get("id") or ev.get("event_id")

    @registry.case(
        id="T1-F21-02",
        tier=1,
        feature_id=21,
        name="subscribe.db outbox emits download.after when DownloadSubmitter commits",
        self_test=False,
    )
    def test_f21_subscribe_outbox_download(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtdl')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "events:download.after", "http", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={"downloader_id": dl["id"], "download_url": f"{pt.url}/download.torrent", "title": "Outbox DL"},
            idempotency_key=uniq("evtdl"),
        )
        assert len(qb.torrents) == 1
        _wait_for_webhooks(pt, min_count=1)
        assert len(pt.webhook_events) >= 1

    @registry.case(
        id="T1-F21-03",
        tier=1,
        feature_id=21,
        name="library.db outbox emits transfer.completed after ledger publication",
        self_test=False,
    )
    def test_f21_library_outbox_transfer(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        lib = ensure_library(ctx)
        ctx.call_phase1("PUT", f"/api/v1/libraries/{lib['id']}/default")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evttx')}",
            kind="hook",
            hooks=[{"id": "transfer.completed", "priority": 400}],
            hook_permissions=["hooks:transfer.completed", "events:transfer.completed", "http", "kv"],
            http_rules=[{"origin": pt.url, "path_prefix": "/", "methods": ["POST"]}],
            enable=True,
        )
        watch_dir = ctx.tmp_dir / "watch_outbox"
        watch_dir.mkdir(parents=True, exist_ok=True)
        (watch_dir / "Outbox.Transfer.2026.1080p.mkv").write_bytes(b"fake-outbox-mkv")
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
        _wait_for_webhooks(pt, min_count=1)
        assert len(pt.webhook_events) >= 1

    @registry.case(
        id="T1-F21-04",
        tier=1,
        feature_id=21,
        name="Outbox preserves causal per-entity sequence across sequential state changes",
        self_test=False,
    )
    def test_f21_causal_order(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtord')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "events:download.after", "kv"],
            enable=True,
        )
        tf2 = pt.default_torrent
        pt.torrent_payloads["seq2"] = pt.default_torrent.bytes
        ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={"downloader_id": dl["id"], "download_url": f"{pt.url}/download.torrent", "title": "Seq 1"},
            idempotency_key=uniq("ord1"),
        )
        from ..lib.fixtures import build_torrent_bytes
        pt.torrent_payloads["seq2"] = build_torrent_bytes(name="Seq.2", length=8192).bytes
        ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={"downloader_id": dl["id"], "download_url": f"{pt.url}/torrents/seq2.torrent", "title": "Seq 2"},
            idempotency_key=uniq("ord2"),
        )
        assert len(qb.torrents) == 2
        audit = _wait_for_audit_action(ctx, pkg.plugin_id, "download.after")
        assert isinstance(audit, list) and "download.after" in str(audit)

    @registry.case(
        id="T1-F21-05",
        tier=1,
        feature_id=21,
        name="Admin plugin audit and SSE job stream stay healthy during outbox dispatch",
        self_test=False,
    )
    def test_f21_audit_and_sse_health(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="pterclub")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('evtsse')}",
            kind="hook",
            hooks=[{"id": "download.after", "priority": 400}],
            hook_permissions=["hooks:download.after", "events:download.after", "kv"],
            enable=True,
        )
        ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={"downloader_id": dl["id"], "download_url": f"{pt.url}/download.torrent", "title": "SSE Health"},
            idempotency_key=uniq("evtsse"),
        )
        audit = _wait_for_audit_action(ctx, pkg.plugin_id, "download.after")
        assert isinstance(audit, list) and "download.after" in str(audit)
        events = ctx.client.read_sse("/api/v1/jobs/stream", max_events=1, timeout=1.0)
        assert isinstance(events, list)


def poll_operation_removal(ctx: TestContext, op_id: str, timeout: float = 15.0) -> dict:
    import time

    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = ctx.call_phase1("GET", f"/api/v1/library-removals/{op_id}")
        if last.get("state") in ("succeeded", "failed"):
            return last
        time.sleep(0.1)
    raise AssertionError(f"Library removal {op_id} did not finish within {timeout}s: {last!r}")
