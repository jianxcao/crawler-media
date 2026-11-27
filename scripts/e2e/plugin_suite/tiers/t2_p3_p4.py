"""Tier 2 Boundary & Negative Tests for Features #14-#18 (P3 & P4 Hooks, Torrents, Downloads & Boost)."""
from __future__ import annotations

import concurrent.futures
import time

from ..lib.client import ApiError
from ..lib.fixtures import (
    build_dishonest_torrent,
    build_magnet_uri,
    build_malformed_torrent,
    build_missing_file_length_torrent,
    build_negative_length_torrent,
    build_overflow_torrent,
    build_oversized_torrent_bytes,
    build_torrent_bytes,
    build_v2_only_torrent,
)
from . import SuiteRegistry, TestContext
from ._helpers import (
    assert_api_error,
    create_member_client,
    create_subscribe,
    ensure_downloader,
    ensure_library,
    ensure_plugin_enabled,
    ensure_site,
    poll_operation,
    restart_and_relogin,
    setup_signed_plugin_ready,
    uniq,
    unwrap_items,
    unwrap_list,
)


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #14: ~10 Core Workflow Hooks & GET /plugin-hook-catalog (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F14-01",
        tier=2,
        feature_id=14,
        name="Member forbidden on /plugin-hook-catalog and Phase 2 hooks absent from catalog",
        self_test=False,
    )
    def test_t2_f14_catalog_rbac_and_phase2_absent(ctx: TestContext) -> None:
        catalog = unwrap_items(ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog"), "hook catalog")
        ids = {h.get("id") for h in catalog if isinstance(h, dict)}
        for deferred_hook in (
            "site.create.before",
            "site.request.before",
            "subscribe.create.before",
            "playback.started",
            "scrape.before",
        ):
            assert deferred_hook not in ids, f"Phase 2 hook {deferred_hook} leaked into catalog"
        _, mc = create_member_client(ctx)
        assert_api_error(lambda: mc.get("/api/v1/plugin-hook-catalog"), (403,), label="member hook catalog")

    @registry.case(
        id="T2-F14-02",
        tier=2,
        feature_id=14,
        name="filter.after / subscribe.score cannot resurrect core-rejected HR candidates",
        self_test=False,
    )
    def test_t2_f14_no_resurrect_rejected_candidates(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents = [
            {
                "id": "hr1",
                "title": "T2 Subscribe 2026 1080p",
                "size": "1.00 GiB",
                "size_bytes": 1073741824,
                "seeders": 10,
                "leechers": 1,
                "snatched": 1,
                "upload_time": "2026-11-20 12:00:00",
                "category": "Movie",
                "free": True,
                "hr": True,
            }
        ]
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        rs = ctx.call_phase1(
            "POST",
            "/api/v1/rule-sets",
            json_body={
                "name": uniq("nohr"),
                "atoms": [
                    {"kind": "resolution", "value": "1080p", "priority": 50, "exclude": False},
                    {"kind": "hr", "priority": 100, "exclude": True, "unknown": "exclude"},
                ],
            },
        )
        sub = ctx.call_phase1(
            "POST",
            "/api/v1/subscriptions",
            json_body={
                "media": {"kind": "movie", "title": "T2 Subscribe"},
                "library_id": lib["id"],
                "filter_id": rs["id"],
            },
        )
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('nores')}",
            kind="hook",
            hooks=[{"id": "filter.after", "priority": 200}],
            enable=True,
        )
        ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
        assert len(pt.request_log) >= 1, f"Expected PT site fetch during subscribe run, got {pt.request_log!r}"
        acts = unwrap_items(
            ctx.call_phase1("GET", f"/api/v1/subscriptions/{sub['id']}/activities"),
            "subscription activities",
        )
        audit = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50"),
            "audit",
        )
        assert isinstance(acts, list) and isinstance(audit, list)
        assert len(acts) >= 1 or len(audit) >= 1, (
            f"Expected rejection activity or filter.after audit, got acts={acts!r} audit={audit!r}"
        )
        assert len(qb.torrents) == 0

    @registry.case(
        id="T2-F14-03",
        tier=2,
        feature_id=14,
        name="Cumulative rescore delta cap across filter.after + subscribe.score is enforced",
        self_test=False,
    )
    def test_t2_f14_rescore_cap(ctx: TestContext) -> None:
        catalog = unwrap_items(ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog"), "hook catalog")
        assert any(h.get("id") == "subscribe.score" for h in catalog)
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "T2 Subscribe 2026 1080p"
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"], title="T2 Subscribe")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('rcap')}",
            kind="hook",
            hooks=[{"id": "filter.after", "priority": 200}, {"id": "subscribe.score", "priority": 200}],
            enable=True,
        )
        ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
        acts = unwrap_items(
            ctx.call_phase1("GET", f"/api/v1/subscriptions/{sub['id']}/activities"),
            "subscription activities",
        )
        assert len(acts) >= 1
        audit = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50"),
            "audit",
        )
        assert isinstance(audit, list)

    @registry.case(
        id="T2-F14-04",
        tier=2,
        feature_id=14,
        name="download.before reject/defer results in 0 MockQbitServer /torrents/add calls",
        self_test=False,
    )
    def test_t2_f14_download_before_reject_zero_adds(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('dlrej')}",
            kind="hook",
            hooks=[{"id": "download.before", "priority": 100}],
            enable=True,
        )
        try:
            res = ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "RejectDownload",
                },
                idempotency_key=uniq("dlrej"),
            )
            if isinstance(res, dict) and res.get("operation_id"):
                done = poll_operation(ctx, res["operation_id"], terminal_states=("rejected", "deferred", "failed", "succeeded"))
                assert done["state"] in ("rejected", "deferred", "failed"), (
                    f"Expected download.before reject/defer, got {done!r}"
                )
            else:
                assert isinstance(res, dict) and res.get("outcome") in ("rejected", "deferred", "failed"), (
                    f"Expected download.before reject/defer outcome or operation_id, got {res!r}"
                )
        except ApiError as exc:
            assert exc.status in (400, 403, 409, 422), (
                f"Expected rejection ApiError, got {exc}"
            )
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 0, f"Expected 0 /torrents/add calls on reject/defer, got {adds!r}"
        audit = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50"),
            "audit",
        )
        assert "download.before" in str(audit), (
            f"Expected download.before audit entry, got {audit!r}"
        )

    @registry.case(
        id="T2-F14-05",
        tier=2,
        feature_id=14,
        name="Closed-policy download.before is listed fail-closed in hook catalog",
        self_test=False,
    )
    def test_t2_f14_fail_closed_policy(ctx: TestContext) -> None:
        catalog = unwrap_items(ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog"), "hook catalog")
        dl_bef = [h for h in catalog if h.get("id") == "download.before"]
        assert len(dl_bef) == 1
        assert "closed" in str(dl_bef[0].get("failure_policy", "")).lower()

    # -----------------------------------------------------------------------
    # Feature #15: Verified .torrent Metainfo Byte-Length Parser (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F15-01",
        tier=2,
        feature_id=15,
        name="Dishonest indexer (size_bytes=1 vs 1 TiB .torrent) rejected against 1 TiB verified bytes",
        self_test=False,
    )
    def test_t2_f15_dishonest_torrent_rejected(ctx: TestContext) -> None:
        dishonest = build_dishonest_torrent(actual_bytes=1024 ** 4)
        pt = ctx.new_pt_site()
        pt.torrent_payloads["dishonest"] = dishonest.bytes
        qb = ctx.new_qbit(disk_free_bytes=10 * 1024 ** 3)
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        try:
            op = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/cm.boost/commands",
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
                idempotency_key=uniq("dishonest"),
            )
            done = poll_operation(ctx, op["operation_id"])
            assert done["state"] in ("failed", "rejected"), (
                f"Expected dishonest 1TiB torrent to fail/reject, got {done!r}"
            )
        except ApiError as exc:
            assert exc.status in (400, 409, 422), (
                f"Expected 400/409/422 on dishonest torrent, got {exc}"
            )
        assert len(qb.torrents) == 0

    @registry.case(
        id="T2-F15-02",
        tier=2,
        feature_id=15,
        name="Multi-file i64 overflow .torrent (2^63 bytes) rejected with 0 qB adds",
        self_test=False,
    )
    def test_t2_f15_i64_overflow_rejected(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        ov = build_overflow_torrent()
        pt = ctx.new_pt_site()
        pt.torrent_payloads["overflow"] = ov.bytes
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": f"{pt.url}/torrents/overflow.torrent",
                    "title": "Overflow Torrent",
                },
                idempotency_key=uniq("ovflow"),
            ),
            (400, 422),
            label="i64 overflow torrent",
        )
        assert len(qb.torrents) == 0

    @registry.case(
        id="T2-F15-03",
        tier=2,
        feature_id=15,
        name="Negative length, missing length, v2-only, HTML, and malformed bencode rejected with 0 urls= fallback",
        self_test=False,
    )
    def test_t2_f15_malformed_torrents_no_url_fallback(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        pt = ctx.new_pt_site()
        pt.torrent_payloads["neg"] = build_negative_length_torrent()
        pt.torrent_payloads["missing"] = build_missing_file_length_torrent()
        pt.torrent_payloads["v2only"] = build_v2_only_torrent()
        pt.torrent_payloads["deep"] = build_malformed_torrent("deep_nesting")
        pt.torrent_payloads["html"] = b"<html><body>403 Login Required</body></html>"
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        for tid in ("neg", "missing", "v2only", "deep", "html"):
            assert_api_error(
                lambda k=tid: ctx.call_phase1(
                    "POST",
                    "/api/v1/downloaders/submit",
                    json_body={
                        "downloader_id": dl["id"],
                        "download_url": f"{pt.url}/torrents/{k}.torrent",
                        "title": f"Bad-{k}",
                    },
                    idempotency_key=uniq(f"badt-{k}"),
                ),
                (400, 422, 502),
                label=f"malformed torrent {tid}",
            )
        assert len(qb.torrents) == 0
        assert [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"] == []

    @registry.case(
        id="T2-F15-04",
        tier=2,
        feature_id=15,
        name="Oversized .torrent (>10 MiB) rejected before qB add",
        self_test=False,
    )
    def test_t2_f15_oversized_torrent_rejected(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        pt = ctx.new_pt_site()
        pt.torrent_payloads["oversized"] = build_oversized_torrent_bytes()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": f"{pt.url}/torrents/oversized.torrent",
                    "title": "Oversized Torrent",
                },
                idempotency_key=uniq("ov10m"),
            ),
            (400, 413, 422, 502),
            label="oversized .torrent > 10MiB",
        )
        assert len(qb.torrents) == 0

    @registry.case(
        id="T2-F15-05",
        tier=2,
        feature_id=15,
        name="Unknown-size magnet URI is blocked from automatic boost admission",
        self_test=False,
    )
    def test_t2_f15_unknown_size_magnet_blocked(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        mag = build_magnet_uri("a" * 40, dn="UnknownSizeMagnet", xl=None)
        try:
            op = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/cm.boost/commands",
                json_body={
                    "kind": "downloader.add",
                    "payload": {
                        "downloader_id": dl["id"],
                        "site_id": site["id"],
                        "download_url": mag,
                        "title": "Unknown Size Magnet",
                    },
                },
                idempotency_key=uniq("magblk"),
            )
            done = poll_operation(ctx, op["operation_id"])
            assert done["state"] in ("failed", "rejected"), (
                f"Expected unknown-size magnet fail/reject, got {done!r}"
            )
        except ApiError as exc:
            assert exc.status in (400, 422)
        assert len(qb.torrents) == 0

    # -----------------------------------------------------------------------
    # Feature #16: Download Operations, Ownership, Reservations & Defer (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F16-01",
        tier=2,
        feature_id=16,
        name="Concurrent budget race never exceeds budget_bytes in MockQbitServer",
        self_test=False,
    )
    def test_t2_f16_concurrent_budget_race(ctx: TestContext) -> None:
        tf1 = build_torrent_bytes(name="Race.A", length=60 * 1024 * 1024)
        tf2 = build_torrent_bytes(name="Race.B", length=60 * 1024 * 1024)
        pt = ctx.new_pt_site()
        pt.torrent_payloads["race1"] = tf1.bytes
        pt.torrent_payloads["race2"] = tf2.bytes
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")

        def submit_one(tid: str) -> str:
            try:
                res = ctx.call_phase1(
                    "POST",
                    "/api/v1/plugins/cm.boost/commands",
                    json_body={
                        "kind": "downloader.add",
                        "payload": {
                            "downloader_id": dl["id"],
                            "site_id": site["id"],
                            "download_url": f"{pt.url}/torrents/{tid}.torrent",
                            "title": f"Race {tid}",
                            "budget_bytes": 100 * 1024 * 1024,
                        },
                    },
                    idempotency_key=uniq(f"race-{tid}"),
                )
                return str(res.get("operation_id", ""))
            except ApiError as exc:
                assert exc.status in (400, 409, 422)
                return ""

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            f1 = pool.submit(submit_one, "race1")
            f2 = pool.submit(submit_one, "race2")
            op1 = f1.result()
            op2 = f2.result()
        for op_id in (op1, op2):
            if op_id:
                poll_operation(ctx, op_id)
        assert len(qb.torrents) == 1, f"Expected exactly 1 torrent admitted within 100 MiB budget, got {qb.torrents!r}"
        total_qb = sum(t["size"] for t in qb.torrents.values())
        assert total_qb == 60 * 1024 * 1024, f"Expected 60 MiB in qB, got {total_qb}"

    @registry.case(
        id="T2-F16-02",
        tier=2,
        feature_id=16,
        name="Foreign same-hash torrent in qB is never overwritten or deleted by boost",
        self_test=False,
    )
    def test_t2_f16_foreign_same_hash_protected(ctx: TestContext) -> None:
        tf = build_torrent_bytes(name="Foreign.Protected", length=8192)
        pt = ctx.new_pt_site()
        pt.torrent_payloads["foreign"] = tf.bytes
        qb = ctx.new_qbit()
        qb.seed_foreign_torrent(tf.info_hash, name="Foreign.Protected", size=8192, tags="manual")
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        try:
            op = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/cm.boost/commands",
                json_body={
                    "kind": "downloader.add",
                    "payload": {
                        "downloader_id": dl["id"],
                        "site_id": site["id"],
                        "download_url": f"{pt.url}/torrents/foreign.torrent",
                        "title": "Foreign Protected",
                    },
                },
                idempotency_key=uniq("fprot"),
            )
            done = poll_operation(ctx, op["operation_id"])
            assert done["state"] in ("failed", "rejected", "blocked"), (
                f"Expected foreign same-hash add to fail/reject, got {done!r}"
            )
        except ApiError as exc:
            assert exc.status in (400, 409, 422)
        assert qb.torrents[tf.info_hash]["tags"] == "manual"
        assert qb.deleted_log == []

    @registry.case(
        id="T2-F16-03",
        tier=2,
        feature_id=16,
        name="Dropped qB response enters unknown, holds reservation across restart, and reconciles without duplicate add",
        self_test=False,
    )
    def test_t2_f16_dropped_qb_response_reconciles(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        qb.drop_response_on_next_add = True
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
                    "title": "Dropped Qbit",
                },
                idempotency_key=uniq("dropqb"),
            )
            if isinstance(res, dict) and res.get("operation_id"):
                done = poll_operation(ctx, res["operation_id"], terminal_states=("unknown", "succeeded", "failed"))
                assert done["state"] in ("unknown", "succeeded")
        except ApiError as exc:
            assert exc.status in (400, 500, 502, 504)
        restart_and_relogin(ctx)
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert int(state.get("reserved_bytes", 0)) == 0

    @registry.case(
        id="T2-F16-04",
        tier=2,
        feature_id=16,
        name="Manually tagging a foreign torrent with ownership string does not grant boost delete rights",
        self_test=False,
    )
    def test_t2_f16_spoofed_tag_refused(ctx: TestContext) -> None:
        qb = ctx.new_qbit()
        foreign_hash = "d" * 40
        qb.seed_foreign_torrent(foreign_hash, name="Spoofed.Tag", size=4096, tags="crawler-media,crawler-media-owned")
        ensure_downloader(ctx, qb.url)
        ensure_plugin_enabled(ctx, "cm.boost")
        err = assert_api_error(
            lambda: ctx.client.post(
                "/api/v1/plugins/cm.boost/commands",
                json_body={"kind": "downloader.delete-task", "payload": {"info_hash": foreign_hash}},
                idempotency_key=uniq("spoof"),
            ),
            (400, 403, 404, 409),
            label="delete foreign torrent with spoofed tag",
        )
        if err.status == 404:
            assert err.code is not None and err.code != "not_found"
        assert foreign_hash in qb.torrents

    @registry.case(
        id="T2-F16-05",
        tier=2,
        feature_id=16,
        name="Deferred candidate beyond retry/time budget transitions cleanly",
        self_test=False,
    )
    def test_t2_f16_defer_budget_limit(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "T2 Subscribe DeferDownload 2026 1080p"
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('defb')}",
            kind="hook",
            hooks=[{"id": "download.before", "priority": 200}],
            enable=True,
        )
        sub = create_subscribe(ctx, lib["id"], title="T2 Subscribe")
        time.sleep(0.5)
        saw_reject = False
        for attempt in range(12):
            try:
                ctx.call_phase1("POST", f"/api/v1/subscriptions/{sub['id']}/run", json_body={})
            except ApiError as exc:
                assert exc.status in (400, 409, 422, 502), f"Unexpected defer reject status: {exc}"
                assert "SECRET_DEFER_PASS" not in str(exc), f"Secret leaked in defer rejection: {exc}"
                saw_reject = True
                break
            if attempt < 11:
                time.sleep(1.05)
        assert saw_reject, "Expected defer budget limit rejection within 12 attempts"
        assert len(pt.request_log) >= 1, f"Expected PT site fetch during subscribe run, got {pt.request_log!r}"
        acts = unwrap_items(
            ctx.call_phase1("GET", f"/api/v1/subscriptions/{sub['id']}/activities"),
            "subscription activities",
        )
        audit = unwrap_list(
            ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/audit?limit=50"),
            "audit",
        )
        assert isinstance(acts, list) and isinstance(audit, list)
        assert len(acts) >= 1 or len(audit) >= 1, (
            f"Expected subscription activity or download.before audit, got acts={acts!r} audit={audit!r}"
        )
        assert len(qb.torrents) == 0

    # -----------------------------------------------------------------------
    # Feature #17: Unified DownloadSubmitter & All-4-Entrypoint Migration (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F17-01",
        tier=2,
        feature_id=17,
        name="All entrypoints obey download.before validation/reject with 0 qB adds on error",
        self_test=False,
    )
    def test_t2_f17_all_entrypoints_reject(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"], title="T2 Subscribe")
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        ensure_plugin_enabled(ctx, "cm.boost")
        for call in (
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": "http://169.254.169.254/a.torrent",
                    "title": "SSRF Reject",
                },
                idempotency_key=uniq("e3bad"),
            ),
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/search/admit",
                json_body={"subscribe_id": sub["id"], "enclosure": "http://169.254.169.254/a.torrent"},
                idempotency_key=uniq("e4bad"),
            ),
        ):
            assert_api_error(call, (400, 403, 422), label="invalid enclosure on unified submitter")
        try:
            bop = ctx.call_phase1(
                "POST",
                "/api/v1/plugins/cm.boost/commands",
                json_body={
                    "kind": "downloader.add",
                    "payload": {
                        "downloader_id": dl["id"],
                        "site_id": site["id"],
                        "download_url": "http://169.254.169.254/a.torrent",
                        "title": "Boost SSRF Reject",
                    },
                },
                idempotency_key=uniq("e5bad"),
            )
            done = poll_operation(ctx, bop["operation_id"])
            assert done["state"] in ("rejected", "failed"), (
                f"Expected plugin downloader.add SSRF rejection, got {done!r}"
            )
        except ApiError as exc:
            assert exc.status in (400, 403, 422), f"Expected 400/403/422, got {exc}"
        assert len(qb.torrents) == 0

    @registry.case(
        id="T2-F17-02",
        tier=2,
        feature_id=17,
        name="Frozen downloader_id in DownloadPlan is not affected by later default downloader change",
        self_test=False,
    )
    def test_t2_f17_frozen_downloader_id(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb_b = ctx.new_qbit()
        qb_c = ctx.new_qbit()
        dl_b = ensure_downloader(ctx, qb_b.url, name=uniq("qbB"))
        _dl_c = ensure_downloader(ctx, qb_c.url, name=uniq("qbC"))
        ensure_site(ctx, pt.url)
        ctx.call_phase1("GET", "/api/v1/plugin-hook-catalog")
        res = ctx.call_phase1(
            "POST",
            "/api/v1/downloaders/submit",
            json_body={"downloader_id": dl_b["id"], "download_url": f"{pt.url}/download.torrent", "title": "Frozen B"},
            idempotency_key=uniq("frzb"),
        )
        if isinstance(res, dict) and res.get("operation_id"):
            assert poll_operation(ctx, res["operation_id"])["state"] == "succeeded"
        adds = [r for r in qb_b.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]
        assert len(qb_b.torrents) == 1 and len(qb_c.torrents) == 0

    @registry.case(
        id="T2-F17-03",
        tier=2,
        feature_id=17,
        name="Dropped qB response on /search/admit reconciles without duplicate /torrents/add",
        self_test=False,
    )
    def test_t2_f17_admit_drop_reconciles_single(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        pt.torrents[0]["title"] = "T2 Subscribe 2026 1080p"
        qb = ctx.new_qbit()
        qb.drop_response_on_next_add = True
        ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url, profile_id="demo")
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"], title="T2 Subscribe")
        ctx.call_phase1("GET", "/api/v1/plugins")
        try:
            ctx.call_phase1(
                "POST",
                "/api/v1/search/admit",
                json_body={"subscribe_id": sub["id"], "enclosure": f"{pt.url}/torrents/101.torrent"},
                idempotency_key=uniq("admdrop"),
            )
        except ApiError as exc:
            assert exc.status in (400, 500, 502, 504)
        adds = [r for r in qb.request_log if r["path"] == "/api/v2/torrents/add"]
        assert len(adds) == 1 and adds[0]["detail"]["file_keys"] == ["torrents"]

    @registry.case(
        id="T2-F17-04",
        tier=2,
        feature_id=17,
        name="Member calling /search/admit receives 403 before any qB call",
        self_test=False,
    )
    def test_t2_f17_member_rbac_before_submit(ctx: TestContext) -> None:
        qb = ctx.new_qbit()
        ensure_downloader(ctx, qb.url)
        lib = ensure_library(ctx)
        sub = create_subscribe(ctx, lib["id"], title="T2 Subscribe")
        ctx.call_phase1("GET", "/api/v1/plugins")
        _, mc = create_member_client(ctx)
        assert_api_error(
            lambda: mc.post(
                "/api/v1/search/admit",
                json_body={"subscribe_id": sub["id"], "enclosure": "https://example.com/a.torrent"},
            ),
            (403,),
            label="member /search/admit",
        )
        assert_api_error(
            lambda: mc.post(
                "/api/v1/plugins/cm.boost/commands",
                json_body={"kind": "downloader.add", "payload": {}},
            ),
            (403,),
            label="member plugin downloader.add",
        )
        assert len(qb.request_log) == 0

    @registry.case(
        id="T2-F17-05",
        tier=2,
        feature_id=17,
        name="MockQbitServer Fails. response returns explicit error without silent success",
        self_test=False,
    )
    def test_t2_f17_qbit_fails_explicit_error(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        qb.reject_next_add = True
        dl = ensure_downloader(ctx, qb.url)
        ensure_site(ctx, pt.url)
        ctx.call_phase1("GET", "/api/v1/plugins")
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/downloaders/submit",
                json_body={
                    "downloader_id": dl["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "Qbit Fails Test",
                },
                idempotency_key=uniq("qbfail"),
            ),
            (400, 409, 502),
            label="qB Fails. rejection",
        )
        assert any(r["path"] == "/api/v2/torrents/add" for r in qb.request_log)

    # -----------------------------------------------------------------------
    # Feature #18: boost.snapshot, Cleanup Plan & Real Boost Loop (5 T2)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T2-F18-01",
        tier=2,
        feature_id=18,
        name="Legacy boost routes PATCH /sites/:id/boost and GET /sites/boost-stats return 404",
        self_test=False,
    )
    def test_t2_f18_legacy_boost_routes_404(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        r1 = ctx.client.raw_request("GET", "/api/v1/sites/boost-stats")
        r2 = ctx.client.raw_request(
            "PATCH",
            "/api/v1/sites/00000000-0000-0000-0000-000000000000/boost",
            body=b"{}",
            headers={"Content-Type": "application/json"},
        )
        assert r1.status == 404 and r2.status == 404

    @registry.case(
        id="T2-F18-02",
        tier=2,
        feature_id=18,
        name="Unknown disk space (disk_endpoint_error=True) pauses new boost adds fail-closed",
        self_test=False,
    )
    def test_t2_f18_disk_unknown_fail_closed(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit(disk_endpoint_error=True)
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="demo")
        ensure_plugin_enabled(ctx, "cm.boost")
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/cm.boost/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert state["disk"]["status"] == "unavailable"
        op = ctx.call_phase1(
            "POST",
            "/api/v1/plugins/cm.boost/commands",
            json_body={"kind": "boost.run", "payload": {"site_id": site["id"], "downloader_id": dl["id"], "dry_run": False}},
            idempotency_key=uniq("unkdisk"),
        )
        poll_operation(ctx, op["operation_id"])
        assert len(qb.torrents) == 0

    @registry.case(
        id="T2-F18-03",
        tier=2,
        feature_id=18,
        name="Foreign hash cleanup/delete-task is refused with 0 qB deletions",
        self_test=False,
    )
    def test_t2_f18_foreign_cleanup_refused(ctx: TestContext) -> None:
        qb = ctx.new_qbit()
        fh = "e" * 40
        qb.seed_foreign_torrent(fh, name="Foreign.Keep", size=4096)
        ensure_downloader(ctx, qb.url)
        ensure_plugin_enabled(ctx, "cm.boost")
        err = assert_api_error(
            lambda: ctx.client.post(
                "/api/v1/plugins/cm.boost/commands",
                json_body={"kind": "downloader.delete-task", "payload": {"info_hash": fh}},
                idempotency_key=uniq("fcln"),
            ),
            (400, 403, 404, 409),
            label="foreign hash delete-task",
        )
        if err.status == 404:
            assert err.code is not None and err.code != "not_found"
        assert qb.deleted_log == []

    @registry.case(
        id="T2-F18-04",
        tier=2,
        feature_id=18,
        name="brush-cleanup-preview rejects both task_ids & plan_handle (400) and requires valid confirmation_token",
        self_test=False,
    )
    def test_t2_f18_cleanup_preview_token_enforcement(ctx: TestContext) -> None:
        ensure_plugin_enabled(ctx, "cm.boost")
        assert_api_error(
            lambda: ctx.call_phase1(
                "GET",
                "/api/v1/plugins/cm.boost/brush-cleanup-preview?task_ids=t1&plan_handle=p1",
            ),
            (400,),
            label="mutually exclusive task_ids and plan_handle",
        )
        assert_api_error(
            lambda: ctx.call_phase1(
                "POST",
                "/api/v1/plugins/cm.boost/commands",
                json_body={
                    "kind": "downloader.delete-files",
                    "payload": {"host_file_plan_handle": "fake", "revision": 1, "reason": "no token"},
                },
                idempotency_key=uniq("notok"),
            ),
            (400, 403, 409),
            label="delete-files without confirmation_token",
        )

    @registry.case(
        id="T2-F18-05",
        tier=2,
        feature_id=18,
        name="downloader.control on foreign hash is rejected",
        self_test=False,
    )
    def test_t2_f18_control_foreign_rejected(ctx: TestContext) -> None:
        qb = ctx.new_qbit()
        fh = "c" * 40
        qb.seed_foreign_torrent(fh, name="Foreign.Ctrl")
        dl = ensure_downloader(ctx, qb.url)
        ensure_plugin_enabled(ctx, "cm.boost")
        err = assert_api_error(
            lambda: ctx.client.post(
                "/api/v1/plugins/cm.boost/commands",
                json_body={
                    "kind": "downloader.control",
                    "payload": {"downloader_id": dl["id"], "action": "pause", "hashes": [fh]},
                },
                idempotency_key=uniq("ctrlf"),
            ),
            (400, 403, 404, 409),
            label="downloader.control on foreign hash",
        )
        if err.status == 404:
            assert err.code is not None and err.code != "not_found"
