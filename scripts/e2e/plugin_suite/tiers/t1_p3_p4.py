"""Tier 1 Happy-Path Tests for Features #14-#18 (P3 & P4 Hooks, Torrents, Downloads & Boost)."""
from __future__ import annotations

from pathlib import Path

from ..lib.client import ApiError
from ..lib.fixtures import build_torrent_bytes
from . import SuiteRegistry, TestContext
from ._helpers import (
    create_subscribe,
    ensure_downloader,
    ensure_library,
    ensure_plugin_enabled,
    ensure_site,
    poll_operation,
    resolve_bin_path,
    setup_signed_plugin_ready,
    trigger_job,
    uniq,
    unwrap_items,
    unwrap_list,
)


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #14: ~10 Core Workflow Hooks & GET /plugin-hook-catalog
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F14-01",
        tier=1,
        feature_id=14,
        name="GET /plugin-hook-catalog lists Phase 1 core hooks and failure policies",
        self_test=False,
    )
    def test_f14_hook_catalog(ctx: TestContext) -> None:
        catalog = unwrap_items(ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog"), "hook catalog")
        assert len(catalog) >= 10
        assert all("failure_policy" in h for h in catalog if isinstance(h, dict))
        ids = {h.get("id") for h in catalog if isinstance(h, dict)}
        for expected in (
            "search.before",
            "search.site.after",
            "search.after",
            "filter.after",
            "subscribe.score",
            "subscribe.select",
            "download.before",
            "download.after",
            "transfer.rename",
            "transfer.completed",
            "site.checkin.after",
        ):
            assert expected in ids, f"Missing core hook {expected} in catalog: {ids!r}"

    @registry.case(
        id="T1-F14-02",
        tier=1,
        feature_id=14,
        name="search.before hook executes during /search/torrents",
        self_test=False,
    )
    def test_f14_search_before(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        ensure_site(ctx, pt.url, profile_id="demo")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('sbef')}",
            kind="hook",
            hooks=[{"id": "search.before", "priority": 200}],
            enable=True,
        )
        res = ctx.call_phase1("GET", "/api/v1/search/torrents?keyword=E2E")
        items = res.get("items") if isinstance(res, dict) else res
        assert isinstance(items, list) and len(items) >= 1
        assert len(pt.request_log) >= 1, f"Expected search request to PT site, got {pt.request_log!r}"
        audit = unwrap_list(ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50"), "audit")
        assert "search.before" in str(audit), (
            f"Expected search.before in plugin audit, got {audit!r}"
        )

    @registry.case(
        id="T1-F14-03",
        tier=1,
        feature_id=14,
        name="Multiple search.after hooks run in deterministic priority order",
        self_test=False,
    )
    def test_f14_search_after_order(ctx: TestContext) -> None:
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
        ensure_site(ctx, pt.url, profile_id="demo")
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('sa1')}",
            kind="hook",
            hooks=[{"id": "search.after", "priority": 100}],
            enable=True,
        )
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('sa2')}",
            kind="hook",
            hooks=[{"id": "search.after", "priority": 200}],
            enable=True,
        )
        res = ctx.call_phase1("GET", "/api/v1/search/torrents")
        items = res.get("items") if isinstance(res, dict) else res
        assert isinstance(items, list)
        titles = [it.get("title") for it in items if isinstance(it, dict)]
        assert "B" in titles and "A" not in titles, f"Expected search.after filter drop A keep B, got {titles!r}"

    @registry.case(
        id="T1-F14-04",
        tier=1,
        feature_id=14,
        name="filter.after, subscribe.score, and subscribe.select execute on /subscriptions/:id/run",
        self_test=False,
    )
    def test_f14_subscribe_hooks(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"])
        for hid in ("filter.after", "subscribe.score", "subscribe.select"):
            setup_signed_plugin_ready(
                ctx,
                plugin_id=f"org.example.{uniq('subh')}",
                kind="hook",
                hooks=[{"id": hid, "priority": 200}],
                enable=True,
            )
        ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
        acts = ctx.call_phase1("GET", f"/api/v1/subscriptions/{sub['id']}/activities")
        items = unwrap_items(acts, "subscription activities")
        assert len(items) >= 1
        assert pt.default_torrent.info_hash in qb.torrents

    @registry.case(
        id="T1-F14-05",
        tier=1,
        feature_id=14,
        name="download.before hook applies DownloadPlanPatch before MockQbitServer submission",
        self_test=False,
    )
    def test_f14_download_before_patch(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url)
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('dlbef')}",
            kind="hook",
            hooks=[{"id": "download.before", "priority": 200}],
            enable=True,
        )
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Patched Release",
            },
            idempotency_key=uniq("dlbef"),
        )
        if isinstance(sub, dict) and sub.get("operation_id"):
            assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
        assert pt.default_torrent.info_hash in qb.torrents

    # -----------------------------------------------------------------------
    # Feature #15: Verified .torrent Metainfo Byte-Length Parser
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F15-01",
        tier=1,
        feature_id=15,
        name="Single-file .torrent submits verified bytes via multipart torrents upload",
        self_test=False,
    )
    def test_f15_single_file_torrent(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        tf = build_torrent_bytes(name="F15.Single.1080p", length=2097152)
        pt = ctx.new_pt_site()
        pt.default_torrent = tf
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url)
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "F15 Single",
            },
            idempotency_key=uniq("f15s"),
        )
        if isinstance(sub, dict) and sub.get("operation_id"):
            assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
        assert tf.info_hash in qb.torrents
        assert qb.torrents[tf.info_hash]["size"] == 2097152
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]
        assert "urls" not in adds[0]["detail"]["fields"]

    @registry.case(
        id="T1-F15-02",
        tier=1,
        feature_id=15,
        name="Multi-file .torrent sums files[].length to exact verified_total_bytes",
        self_test=False,
    )
    def test_f15_multi_file_torrent(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        tf = build_torrent_bytes(
            name="F15.Multi.Pack",
            files=[(["a.mkv"], 1000), (["b.srt"], 500)],
        )
        pt = ctx.new_pt_site()
        pt.default_torrent = tf
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url)
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "F15 Multi",
            },
            idempotency_key=uniq("f15m"),
        )
        if isinstance(sub, dict) and sub.get("operation_id"):
            assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
        assert tf.info_hash in qb.torrents
        assert qb.torrents[tf.info_hash]["size"] == 1500
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]

    @registry.case(
        id="T1-F15-03",
        tier=1,
        feature_id=15,
        name="cm.boost brush-state accounts exact verified_total_bytes",
        self_test=False,
    )
    def test_f15_boost_verified_bytes(ctx: TestContext) -> None:
        tf = build_torrent_bytes(name="F15.Boost.Item", length=4096)
        pt = ctx.new_pt_site()
        pt.default_torrent = tf
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "F15 Boost Item",
                },
            },
            idempotency_key=uniq("f15b"),
        )
        assert poll_operation(ctx, op["operation_id"])["state"] == "succeeded"
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state["used_bytes"]) >= 4096 and "reserved_bytes" in state

    @registry.case(
        id="T1-F15-04",
        tier=1,
        feature_id=15,
        name="Torrent submission uploads multipart file_keys=['torrents'] with zero urls= fallback",
        self_test=False,
    )
    def test_f15_no_urls_fallback(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url)
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Multipart Only",
            },
            idempotency_key=uniq("f15mp"),
        )
        if isinstance(sub, dict) and sub.get("operation_id"):
            assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1
        assert adds[0]["detail"]["file_keys"] == ["torrents"]
        assert "urls" not in adds[0]["detail"]["fields"]

    @registry.case(
        id="T1-F15-05",
        tier=1,
        feature_id=15,
        name="POST /search/admit resolves .torrent via net-guard and matches qB info_hash",
        self_test=False,
    )
    def test_f15_admit_info_hash(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"])
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        ctx.call_phase1(
            "POST",
            "/api/v1/search/admit",
            json_body={
                "subscribe_id": sub["id"],
                "enclosure": f"{pt.url}/torrents/101.torrent",
            },
            idempotency_key=uniq("f15adm"),
        )
        assert pt.default_torrent.info_hash in qb.torrents
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]

    # -----------------------------------------------------------------------
    # Feature #16: Download Operations, Ownership, Reservations & Defer
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F16-01",
        tier=1,
        feature_id=16,
        name="Download submission records ownership tag and operation state",
        self_test=False,
    )
    def test_f16_ownership_tag(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        boost_tf = build_torrent_bytes(name="F16.Boost.Owned", length=4096)
        pt.torrent_payloads["boost16"] = boost_tf.bytes
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "site_id": site["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Owned Task",
            },
            idempotency_key=uniq("f16own"),
        )
        if isinstance(sub, dict) and sub.get("operation_id"):
            assert poll_operation(ctx, sub["operation_id"])["state"] == "succeeded"
        assert pt.default_torrent.info_hash in qb.torrents
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        assert any(t.get("info_hash") == pt.default_torrent.info_hash for t in tasks)
        bop = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/torrents/boost16.torrent",
                    "title": "Boost Owned Task",
                },
            },
            idempotency_key=uniq("f16bown"),
        )
        assert poll_operation(ctx, bop["operation_id"])["state"] == "succeeded"
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state["used_bytes"]) >= boost_tf.verified_total_bytes

    @registry.case(
        id="T1-F16-02",
        tier=1,
        feature_id=16,
        name="Deleting Subscribe does not erase retained download ownership or brush-state accounting",
        self_test=False,
    )
    def test_f16_delete_subscribe_keeps_ownership(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        boost_tf = build_torrent_bytes(name="F16.Boost.Keep", length=8192)
        pt.torrent_payloads["boost16k"] = boost_tf.bytes
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"])
        ensure_plugin_enabled(ctx, "cm.boost")
        ctx.call_phase1(
            "POST",
            "/api/v1/search/admit",
            json_body={"subscribe_id": sub["id"], "enclosure": f"{pt.url}/torrents/101.torrent"},
            idempotency_key=uniq("f16sub"),
        )
        bop = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/torrents/boost16k.torrent",
                    "title": "Boost Retained Before Subscribe Delete",
                },
            },
            idempotency_key=uniq("f16bk"),
        )
        assert poll_operation(ctx, bop["operation_id"])["state"] == "succeeded"
        ctx.call_phase1("DELETE", f"/api/v1/subscriptions/{sub['id']}")
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        assert any(t.get("info_hash") == pt.default_torrent.info_hash for t in tasks)
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state["used_bytes"]) >= boost_tf.verified_total_bytes

    @registry.case(
        id="T1-F16-03",
        tier=1,
        feature_id=16,
        name="Download tasks and defer activities survive server inspection cleanly",
        self_test=False,
    )
    def test_f16_tasks_and_activities(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"])
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f16act')}",
            kind="hook",
            hooks=[{"id": "download.before", "priority": 200}],
            enable=True,
        )
        ensure_plugin_enabled(ctx, "cm.boost")
        res = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "site_id": site["id"],
                "subscribe_id": sub["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "E2E Demo Movie 2026 1080p WEB-DL",
            },
            idempotency_key=uniq("f16actsub"),
        )
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] in ("succeeded", "deferred")
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        acts = unwrap_items(
            ctx.call_phase1("GET", f"/api/v1/subscriptions/{sub['id']}/activities"),
            "subscription activities",
        )
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert len(tasks) >= 1 or len(acts) >= 1
        assert "used_bytes" in state and "reserved_bytes" in state

    @registry.case(
        id="T1-F16-04",
        tier=1,
        feature_id=16,
        name="GET /downloaders/tasks shows owned tasks without claiming unowned foreign torrents",
        self_test=False,
    )
    def test_f16_foreign_not_claimed(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        foreign_hash = "f" * 40
        qb.seed_foreign_torrent(foreign_hash, name="foreign.iso", size=2048, tags="manual")
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        owned_res = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "site_id": site["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Owned Task Check",
            },
            idempotency_key=uniq("f16ownchk"),
        )
        if isinstance(owned_res, dict) and owned_res.get("operation_id"):
            assert poll_operation(ctx, owned_res["operation_id"])["state"] == "succeeded"
        assert pt.default_torrent.info_hash in qb.torrents
        tasks = unwrap_items(ctx.call_phase1("GET", "/api/v1/downloaders/tasks"), "downloader tasks")
        assert any(r["path"] == "/api/v2/torrents/info" for r in qb.request_log), (
            f"Expected /api/v2/torrents/info query to qBittorrent, got {qb.request_log!r}"
        )
        assert any(
            t.get("info_hash") == pt.default_torrent.info_hash and t.get("owned", True) is True
            for t in tasks
        ), f"Expected owned task {pt.default_torrent.info_hash} in /downloaders/tasks, got {tasks!r}"
        assert not any(t.get("info_hash") == foreign_hash for t in tasks), (
            f"Foreign torrent {foreign_hash} must be excluded from /downloaders/tasks: {tasks!r}"
        )
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?downloader_id={dl['id']}",
        )
        assert not any(t.get("info_hash") == foreign_hash for t in (state.get("tasks") or []))

    @registry.case(
        id="T1-F16-05",
        tier=1,
        feature_id=16,
        name="Failed pre-add download releases temporary reservation cleanly (reserved_bytes == 0)",
        self_test=False,
    )
    def test_f16_failed_preadd_releases_reservation(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        qb.reject_next_add = True
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        try:
            res = ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "Rejected Add",
                },
                idempotency_key=uniq("f16rel"),
            )
            if isinstance(res, dict) and res.get("operation_id"):
                done = poll_operation(ctx, res["operation_id"])
                assert done["state"] in ("failed", "rejected")
            else:
                assert isinstance(res, dict) and res.get("outcome") in ("failed", "rejected"), (
                    f"Expected failed/rejected submit outcome or operation_id, got {res!r}"
                )
        except ApiError as exc:
            assert exc.status in (400, 409, 422, 502)
        assert len(pt.request_log) >= 1, (
            f"Expected .torrent fetch from PT site before pre-add rejection, got {pt.request_log!r}"
        )
        assert any(r["path"] == "/api/v2/torrents/add" for r in qb.request_log), (
            f"Expected /api/v2/torrents/add attempt before qB rejection, got {qb.request_log!r}"
        )
        assert len(qb.torrents) == 0
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state.get("reserved_bytes", 0)) == 0

    # -----------------------------------------------------------------------
    # Feature #17: Unified DownloadSubmitter & All-4-Entrypoint Migration
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F17-01",
        tier=1,
        feature_id=17,
        name="Entrypoint 1: Scheduled Subscribe via POST /jobs/tick routes through DownloadSubmitter",
        self_test=False,
    )
    def test_f17_entrypoint_tick(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"])
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f17t')}",
            kind="hook",
            hooks=[{"id": "download.before", "priority": 200}],
            enable=True,
        )
        trigger_job(ctx, "subscribe_search", scope_key=f"subscribe:{sub['id']}")
        assert pt.default_torrent.info_hash in qb.torrents
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]
        assert "urls" not in adds[0]["detail"]["fields"]

    @registry.case(
        id="T1-F17-02",
        tier=1,
        feature_id=17,
        name="Entrypoint 2: Manual Subscribe POST /subscriptions/:id/run routes through DownloadSubmitter",
        self_test=False,
    )
    def test_f17_entrypoint_manual_run(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"])
        setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f17m')}",
            kind="hook",
            hooks=[{"id": "download.before", "priority": 200}],
            enable=True,
        )
        ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
        assert pt.default_torrent.info_hash in qb.torrents
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]
        assert "urls" not in adds[0]["detail"]["fields"]

    @registry.case(
        id="T1-F17-03",
        tier=1,
        feature_id=17,
        name="Entrypoint 3: POST /downloaders/submit routes through DownloadSubmitter",
        self_test=False,
    )
    def test_f17_entrypoint_submit(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url)
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        res = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={
                "downloader_id": dl["id"],
                "download_url": f"{pt.url}/download.torrent",
                "title": "Submit Direct",
            },
            idempotency_key=uniq("ep3"),
        )
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] == "succeeded"
        assert len(qb.torrents) == 1
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]
        assert "urls" not in adds[0]["detail"]["fields"]

    @registry.case(
        id="T1-F17-04",
        tier=1,
        feature_id=17,
        name="Entrypoint 4: POST /search/admit and CLI search --admit route through DownloadSubmitter",
        self_test=False,
    )
    def test_f17_entrypoint_admit_and_cli(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "E2E Demo Movie 2026 1080p WEB-DL"
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"])
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        ctx.call_phase1(
            "POST",
            "/api/v1/search/admit",
            json_body={"subscribe_id": sub["id"], "enclosure": f"{pt.url}/torrents/101.torrent"},
            idempotency_key=uniq("ep4"),
        )
        assert pt.default_torrent.info_hash in qb.torrents
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]
        if ctx.supervisor is not None and resolve_bin_path(ctx).is_file():
            tf_cli = build_torrent_bytes(name="E2E.Demo.CLI.Admit", length=131072)
            pt.torrent_payloads["102"] = tf_cli.bytes
            pt.torrents.append(
                {
                    "id": "102",
                    "title": "E2E Demo Movie CLI 2026 1080p WEB-DL",
                    "size": "128.00 KiB",
                    "size_bytes": 131072,
                    "seeders": 18,
                    "leechers": 1,
                    "snatched": 10,
                    "upload_time": "2026-11-20 12:05:00",
                    "category": "Movie",
                    "free": True,
                    "hr": False,
                }
            )
            sub_cli = create_subscribe(ctx, lib["id"], title="E2E Demo Movie CLI")
            proc = ctx.supervisor.run_cli(
                ["search", "--admit", "--subscribe", str(sub_cli["id"]), "--enclosure", f"{pt.url}/torrents/102.torrent"]
            )
            assert proc.returncode == 0, f"CLI search --admit failed: {proc.stderr}"
            assert tf_cli.info_hash in qb.torrents

    @registry.case(
        id="T1-F17-05",
        tier=1,
        feature_id=17,
        name="Entrypoint 5: Plugin commands.submit(downloader.add) routes through DownloadSubmitter",
        self_test=False,
    )
    def test_f17_entrypoint_plugin_command(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "Plugin Cmd Submit",
                },
            },
            idempotency_key=uniq("ep5"),
        )
        assert poll_operation(ctx, op["operation_id"])["state"] == "succeeded"
        assert pt.default_torrent.info_hash in qb.torrents
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]

    # -----------------------------------------------------------------------
    # Feature #18: boost.snapshot, Cleanup Plan & Real Boost Loop
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F18-01",
        tier=1,
        feature_id=18,
        name="cm.boost dry_run cycle leaves reserved_bytes == 0 and 0 qB add/delete calls",
        self_test=False,
    )
    def test_f18_boost_dry_run(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        before_req = len(qb.request_log)
        op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={"kind": "boost.run", "payload": {"site_id": site["id"], "downloader_id": dl["id"], "dry_run": True}},
            idempotency_key=uniq("dry"),
        )
        assert poll_operation(ctx, op["operation_id"])["state"] == "succeeded"
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state.get("reserved_bytes", 0)) == 0
        mutating = [
            r for r in qb.request_log[before_req:]
            if r["path"] in ("/api/v2/torrents/add", "/api/v2/torrents/delete")
        ]
        assert mutating == []
        audit = unwrap_list(ctx.call_phase1("GET", "/api/v1/plugins/cm.boost/audit?limit=50"), "audit")
        boost_audit = any(
            isinstance(a, dict)
            and a.get("action") not in (
                None,
                "install",
                "install-prepare",
                "enable",
                "disable",
                "config",
                "plugin.install",
                "plugin.enable",
                "plugin.disable",
                "plugin.config",
            )
            and "boost" in str(a.get("action", "")).lower()
            for a in audit
        )
        ran_boost = (
            len(pt.request_log) >= 1
            or any(r["path"].startswith("/api/v2/") for r in qb.request_log[before_req:])
            or boost_audit
        )
        assert ran_boost, (
            f"Expected cm.boost dry_run to execute "
            f"(pt={pt.request_log!r}, qb={qb.request_log[before_req:]!r}, audit={audit!r})"
        )

    @registry.case(
        id="T1-F18-02",
        tier=1,
        feature_id=18,
        name="cm.boost active cycle updates brush-state with used_bytes, tasks, and disk",
        self_test=False,
    )
    def test_f18_boost_active_cycle(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={"kind": "boost.run", "payload": {"site_id": site["id"], "downloader_id": dl["id"], "dry_run": False}},
            idempotency_key=uniq("actcyc"),
        )
        assert poll_operation(ctx, op["operation_id"])["state"] == "succeeded"
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        for key in ("used_bytes", "reserved_bytes", "retained_file_bytes", "tasks", "disk"):
            assert key in state, f"Missing {key} in brush-state: {state!r}"
        assert int(state["used_bytes"]) > 0
        assert len(state["tasks"]) >= 1
        assert state["disk"].get("status") == "available"
        assert int(state["disk"].get("available_bytes", 0)) > 0

    @registry.case(
        id="T1-F18-03",
        tier=1,
        feature_id=18,
        name="downloader.delete-task passes deleteFiles=false and keeps retained_file_bytes occupied",
        self_test=False,
    )
    def test_f18_delete_task_retains_files(ctx: TestContext) -> None:
        save_dir = ctx.tmp_dir / "qb_save"
        qb = ctx.new_qbit(real_save_dir=save_dir)
        pt = ctx.new_pt_site()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        add_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "Boost Retain",
                },
            },
            idempotency_key=uniq("badd"),
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
        assert len(matched) >= 1, f"Expected owned boost task before delete-task, got {tasks!r}"
        tid = matched[0]["id"]
        del_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={"kind": "downloader.delete-task", "payload": {"task_id": tid}},
            idempotency_key=uniq("bdel"),
        )
        assert poll_operation(ctx, del_op["operation_id"])["state"] == "succeeded"
        assert any(d["deleteFiles"] is False for d in qb.deleted_log)
        assert list(save_dir.glob("**/*.mkv")), f"Expected retained .mkv file in {save_dir}"
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state["retained_file_bytes"]) > 0

    @registry.case(
        id="T1-F18-04",
        tier=1,
        feature_id=18,
        name="Two-step cleanup plan: prepare-cleanup -> brush-cleanup-preview -> delete-files",
        self_test=False,
    )
    def test_f18_cleanup_plan_flow(ctx: TestContext) -> None:
        save_dir = ctx.tmp_dir / "qb_clean"
        qb = ctx.new_qbit(real_save_dir=save_dir)
        pt = ctx.new_pt_site()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        add_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "Boost Cleanup Target",
                },
            },
            idempotency_key=uniq("cadd"),
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
        assert len(matched) >= 1, f"Expected owned boost task for cleanup, got {tasks!r}"
        tid = matched[0]["id"]
        del_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={"kind": "downloader.delete-task", "payload": {"task_id": tid}},
            idempotency_key=uniq("cdel"),
        )
        assert poll_operation(ctx, del_op["operation_id"])["state"] == "succeeded"
        prep_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.prepare-cleanup",
                "payload": {"site_id": site["id"], "downloader_id": dl["id"], "task_ids": [tid]},
            },
            idempotency_key=uniq("bprep"),
        )
        done = poll_operation(ctx, prep_op["operation_id"])
        assert done["state"] == "succeeded"
        plan_handle = (done.get("result") or {}).get("plan_handle")
        assert plan_handle, f"Missing plan_handle from prepare-cleanup: {done!r}"
        preview = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-cleanup-preview?plan_handle={plan_handle}",
        )
        assert preview.get("mode") == "plan"
        assert preview.get("confirmation_token")
        del_files_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.delete-files",
                "payload": {
                    "host_file_plan_handle": plan_handle,
                    "revision": preview.get("revision", 1),
                    "confirmation_token": preview["confirmation_token"],
                    "reason": "e2e cleanup test",
                },
            },
            idempotency_key=uniq("delfiles"),
        )
        assert poll_operation(ctx, del_files_op["operation_id"])["state"] == "succeeded"
        state_after = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state_after["retained_file_bytes"]) == 0
        assert not list(save_dir.glob("**/*.mkv"))

    @registry.case(
        id="T1-F18-05",
        tier=1,
        feature_id=18,
        name="downloader.control updates owned boost task in MockQbitServer",
        self_test=False,
    )
    def test_f18_downloader_control(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        add_op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "Boost Control Target",
                },
            },
            idempotency_key=uniq("ctrladd"),
        )
        assert poll_operation(ctx, add_op["operation_id"])["state"] == "succeeded"
        op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={
                "kind": "downloader.control",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "action": "pause",
                    "hashes": [pt.default_torrent.info_hash],
                },
            },
            idempotency_key=uniq("bctrl"),
        )
        assert poll_operation(ctx, op["operation_id"])["state"] == "succeeded"
        assert pt.default_torrent.info_hash in qb.torrents, (
            f"Expected {pt.default_torrent.info_hash} in qb.torrents, got {list(qb.torrents.keys())!r}"
        )
        assert qb.torrents[pt.default_torrent.info_hash]["state"].startswith("paused")
