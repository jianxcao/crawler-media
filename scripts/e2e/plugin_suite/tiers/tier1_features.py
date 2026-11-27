"""Tier 1: Feature-level opaque-box smoke & feature tests (Features #1-#26)."""
from __future__ import annotations

import uuid

from ..lib.fixtures import (
    build_cmplugin,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
    verify_minisign,
)
from . import SuiteRegistry, TestContext, t1_p1c, t1_p2, t1_p3_p4, t1_p5, t1_p6
from ._helpers import pin_trust_key, uniq, upload_cmplugin


def register_cases(registry: SuiteRegistry) -> None:
    @registry.case(
        id="T1-F08-SMOKE-01",
        tier=1,
        feature_id=8,
        name="Health, admin auth me, plugin list, and Phase 2 deferred routes return 404",
        self_test=True,
    )
    def test_t1_auth_and_phase2_boundary(ctx: TestContext) -> None:
        health = ctx.client.health()
        assert health.get("status") == "ok", f"Unexpected health: {health!r}"

        me = ctx.client.me()
        assert me.get("role") == "admin", f"Expected admin role, got {me!r}"
        assert me.get("id"), f"Expected non-empty user.id in /auth/me: {me!r}"

        plugins = ctx.call_phase1("GET", "/api/v1/plugins")
        assert isinstance(plugins, list), f"Expected list from /plugins, got {type(plugins)}"

        for deferred in (
            "/api/v1/plugin-repos",
            "/api/v1/list-subscriptions",
            "/api/v1/plugins/org.example.none/widgets",
            "/api/v1/plugins/org.example.none/endpoint-keys",
        ):
            resp = ctx.client.raw_request("GET", deferred)
            assert resp.status == 404, (
                f"Phase 2 deferred route {deferred} must return 404, got {resp.status}"
            )

    @registry.case(
        id="T1-F01-SMOKE-02",
        tier=1,
        feature_id=1,
        name="Build signed .cmplugin archive and verify multipart upload + reauth contract",
        self_test=True,
    )
    def test_t1_signed_package_upload(ctx: TestContext) -> None:
        pkg = build_cmplugin(plugin_id="org.example.t1-smoke", version="1.0.0", kind="hook")
        kp = generate_minisign_keypair(seed=uuid.uuid4().bytes + uuid.uuid4().bytes)
        sig_text = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        ver = verify_minisign(pkg.sha256_bytes, sig_text, kp.public_key_text)
        assert ver.valid, f"Local minisign verification failed: {ver.reason}"

        pinned = pin_trust_key(ctx, kp, label=uniq("smoke"))
        assert pinned.get("fingerprint"), f"Missing trust key fingerprint: {pinned!r}"

        upload_id = str(uuid.uuid4())
        uploaded = upload_cmplugin(
            ctx,
            pkg.bytes,
            sig_text=sig_text,
            upload_id=upload_id,
            idempotency_key=f"idem-{upload_id}",
        )
        assert uploaded.get("hash") == pkg.sha256_hex, (
            f"Upload hash mismatch: {uploaded.get('hash')} != {pkg.sha256_hex}"
        )

    t1_p1c.register_cases(registry)
    t1_p2.register_cases(registry)
    t1_p3_p4.register_cases(registry)
    t1_p5.register_cases(registry)
    t1_p6.register_cases(registry)
