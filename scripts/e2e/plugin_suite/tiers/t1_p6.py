"""Tier 1 Happy-Path Tests for Features #22-#26 (P6 UI Platform, CLI & First-Party UIs)."""
from __future__ import annotations

import hashlib
import io
import json
import zipfile

from ..lib.fixtures import (
    build_cmplugin,
    build_secret_config_schema,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
    verify_minisign,
)
from ..lib.instance import InstanceSupervisor, find_repo_root
from . import SuiteRegistry, TestContext, UnimplementedFeatureError
from ._helpers import (
    enable_plugin,
    ensure_downloader,
    ensure_plugin_enabled,
    ensure_site,
    grant_ui_trust,
    make_temp_web_dir,
    obtain_reauth,
    pin_trust_key,
    poll_operation,
    resolve_bin_path,
    run_plugin_cli,
    set_developer_mode,
    setup_signed_plugin_ready,
    uniq,
    unwrap_items,
    upload_cmplugin,
)


def _find_first_party_plugin(ctx: TestContext, preferred_ids: tuple, keyword: str) -> dict:
    plugins = ctx.call_phase1("GET", "/api/v1/plugins")
    for pid in preferred_ids:
        for p in plugins:
            if p.get("id") == pid:
                return p
    for p in plugins:
        if keyword in str(p.get("id", "")).lower():
            return p
    raise UnimplementedFeatureError(f"First-party plugin ({keyword})", "not installed in /api/v1/plugins")


def register_cases(registry: SuiteRegistry) -> None:
    # -----------------------------------------------------------------------
    # Feature #22: Minimal Vite 7 + React 19 @module-federation/vite PoC
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F22-01",
        tier=1,
        feature_id=22,
        name="GET /plugin-ui-contract returns exact contract 1.0.0 and React 19.3.0",
        self_test=False,
    )
    def test_f22_ui_contract(ctx: TestContext) -> None:
        contract = ctx.call_phase1("GET", "/api/v1/plugin-ui-contract")
        assert contract.get("contract") == "1.0.0"
        assert contract.get("react") == "19.3.0"
        assert contract.get("react_dom") == "19.3.0"
        assert contract.get("sdk_methods")

    @registry.case(
        id="T1-F22-02",
        tier=1,
        feature_id=22,
        name="Valid ui-only federation-react .cmplugin passes upload and install prepare",
        self_test=False,
    )
    def test_f22_ui_only_prepare(ctx: TestContext) -> None:
        kp = generate_minisign_keypair(seed=b"\x61" * 32)
        pin_trust_key(ctx, kp, label=uniq("f22"))
        pkg = build_cmplugin(plugin_id=f"org.example.{uniq('fed1')}", kind="ui-only")
        sig = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
        up = upload_cmplugin(ctx, pkg.bytes, sig_text=sig)
        assert up.get("hash") == pkg.sha256_hex
        prep = ctx.call_phase1(
            "POST",
            "/api/v1/plugin-installations/prepare",
            json_body={"upload_id": up["upload_id"]},
            if_match="0",
            idempotency_key=uniq("f22prep"),
        )
        done = poll_operation(ctx, prep["operation_id"])
        assert done["state"] == "succeeded"
        prep_res = done.get("result") or done
        assert prep_res.get("package_hash") == pkg.sha256_hex

    @registry.case(
        id="T1-F22-03",
        tier=1,
        feature_id=22,
        name="Two independent UI remotes serve distinct remoteEntry.js simultaneously without rebuild",
        self_test=False,
    )
    def test_f22_two_remotes(ctx: TestContext) -> None:
        pkg_a, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('feda')}",
            kind="ui-only",
            ui_overrides={"remote_name": uniq("cm_fed_a").replace("-", "_")},
            enable=True,
        )
        pkg_b, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('fedb')}",
            kind="ui-only",
            ui_overrides={"remote_name": uniq("cm_fed_b").replace("-", "_")},
            enable=True,
        )
        grant_ui_trust(ctx, pkg_a.plugin_id, pkg_a.sha256_hex)
        grant_ui_trust(ctx, pkg_b.plugin_id, pkg_b.sha256_hex)
        cookie_cli = ctx.client.clone_cookie_only()
        ra = cookie_cli.get_asset(
            f"/api/v1/plugin-ui-assets/{pkg_a.plugin_id}/{pkg_a.sha256_hex}/ui/assets/remoteEntry.js"
        )
        rb = cookie_cli.get_asset(
            f"/api/v1/plugin-ui-assets/{pkg_b.plugin_id}/{pkg_b.sha256_hex}/ui/assets/remoteEntry.js"
        )
        assert ra.status == 200 and rb.status == 200
        assert ra.body != rb.body

    @registry.case(
        id="T1-F22-04",
        tier=1,
        feature_id=22,
        name="Combined task-plugin + React UI bundle (.js/.mjs/.css/.png/.webp) installs and exposes UI",
        self_test=False,
    )
    def test_f22_combined_wasm_and_ui(ctx: TestContext) -> None:
        pkg, _, plugin = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('combo')}",
            kind="task",
            include_ui=True,
            extra_ui_assets={
                "ui/assets/chunk.mjs": (b"export const c = 1;\n", "application/javascript"),
                "ui/assets/logo.png": (b"\x89PNG\r\n\x1a\n", "image/png"),
                "ui/assets/banner.webp": (b"RIFF\x00\x00\x00\x00WEBP", "image/webp"),
            },
            enable=True,
        )
        tasks = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/tasks")
        assert isinstance(tasks, list) and len(tasks) >= 1, (
            f"Expected declared task for {pkg.plugin_id}, got {tasks!r}"
        )
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/ui")
        assert desc.get("plugin_id") == plugin["id"]
        assert desc.get("package_hash") == pkg.sha256_hex
        assert len(desc.get("surfaces") or []) >= 1
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        cookie_cli = ctx.client.clone_cookie_only()
        for rel_path, expected_bytes in (
            ("ui/assets/chunk.mjs", b"export const c = 1;\n"),
            ("ui/assets/logo.png", b"\x89PNG\r\n\x1a\n"),
            ("ui/assets/banner.webp", b"RIFF\x00\x00\x00\x00WEBP"),
        ):
            r = cookie_cli.get_asset(
                f"/api/v1/plugin-ui-assets/{pkg.plugin_id}/{pkg.sha256_hex}/{rel_path}",
                use_cookie=True,
                no_bearer=True,
            )
            assert r.status == 200, f"Expected 200 fetching {rel_path}, got {r.status}"
            assert r.body == expected_bytes, f"Mismatch for {rel_path}: {r.body!r}"

    @registry.case(
        id="T1-F22-05",
        tier=1,
        feature_id=22,
        name="UI contract and first-party/built federation assets exclude duplicate react vendor copy",
        self_test=False,
    )
    def test_f22_shared_react_no_duplicate(ctx: TestContext) -> None:
        contract = ctx.call_phase1("GET", "/api/v1/plugin-ui-contract")
        assert contract.get("react") == "19.3.0"
        checkin_manifest = find_repo_root() / "examples" / "plugins" / "checkin" / "ui" / "asset-manifest.json"
        if checkin_manifest.is_file():
            data = json.loads(checkin_manifest.read_text(encoding="utf-8"))
            assert data.get("version") == 1
            for f in data.get("files", []):
                assert not f["path"].endswith(".html") and not f["path"].endswith(".map")

    # -----------------------------------------------------------------------
    # Feature #23: Author CLI (tools/plugin/)
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F23-01",
        tier=1,
        feature_id=23,
        name="crawler-media plugin scaffold creates plugin.toml, config.schema.json, and React ui/",
        self_test=False,
    )
    def test_f23_cli_scaffold(ctx: TestContext) -> None:
        proj = ctx.tmp_dir / "scaffolded"
        proc = run_plugin_cli(
            ctx,
            ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", "--ui", "react", str(proj)],
        )
        assert proc.returncode == 0, f"scaffold failed: {proc.stderr}"
        assert (proj / "plugin.toml").is_file()
        assert (proj / "schemas" / "config.schema.json").is_file()
        assert (proj / "ui").is_dir()

    @registry.case(
        id="T1-F23-02",
        tier=1,
        feature_id=23,
        name="crawler-media plugin validate exits 0 on valid plugin project directory",
        self_test=False,
    )
    def test_f23_cli_validate(ctx: TestContext) -> None:
        proj = ctx.tmp_dir / "val_proj"
        p1 = run_plugin_cli(ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", str(proj)])
        assert p1.returncode == 0, p1.stderr
        assert (proj / "plugin.toml").is_file(), f"Expected scaffolded plugin.toml in {proj}"
        p2 = run_plugin_cli(ctx, ["validate", str(proj)])
        assert p2.returncode == 0, f"validate failed: {p2.stderr}"

    @registry.case(
        id="T1-F23-03",
        tier=1,
        feature_id=23,
        name="crawler-media plugin test --offline executes without network access",
        self_test=False,
    )
    def test_f23_cli_test_offline(ctx: TestContext) -> None:
        proj = ctx.tmp_dir / "test_proj"
        p1 = run_plugin_cli(ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", str(proj)])
        assert p1.returncode == 0, p1.stderr
        assert (proj / "plugin.toml").is_file(), f"Expected scaffolded plugin.toml in {proj}"
        p2 = run_plugin_cli(ctx, ["test", str(proj), "--offline"])
        assert p2.returncode == 0, f"plugin test --offline failed: {p2.stderr}"

    @registry.case(
        id="T1-F23-04",
        tier=1,
        feature_id=23,
        name="crawler-media plugin build and pack produce valid .cmplugin accepted by server",
        self_test=False,
    )
    def test_f23_cli_build_and_pack(ctx: TestContext) -> None:
        proj = ctx.tmp_dir / "pack_proj"
        out_pkg = ctx.tmp_dir / "out.cmplugin"
        assert run_plugin_cli(ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["build", str(proj)]).returncode == 0
        p_pack = run_plugin_cli(ctx, ["pack", str(proj), "--output", str(out_pkg)])
        assert p_pack.returncode == 0 and out_pkg.is_file(), p_pack.stderr
        set_developer_mode(ctx, True)
        try:
            up = upload_cmplugin(ctx, out_pkg.read_bytes(), sig_text=None)
            assert up.get("hash")
        finally:
            set_developer_mode(ctx, False)

    @registry.case(
        id="T1-F23-05",
        tier=1,
        feature_id=23,
        name="crawler-media plugin sign writes verifiable .minisig and dev sideloads in dev mode",
        self_test=False,
    )
    def test_f23_cli_sign_and_dev(ctx: TestContext) -> None:
        proj = ctx.tmp_dir / "sign_proj"
        out_pkg = ctx.tmp_dir / "signed.cmplugin"
        kp = generate_minisign_keypair(seed=b"\x73" * 32)
        key_file = ctx.tmp_dir / "publisher.key"
        key_file.write_text(kp.secret_seed.hex(), encoding="utf-8")
        assert run_plugin_cli(ctx, ["scaffold", "--world", "hook-plugin", "--api", "1.0.0", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["build", str(proj)]).returncode == 0
        assert run_plugin_cli(ctx, ["pack", str(proj), "--output", str(out_pkg)]).returncode == 0
        p_sign = run_plugin_cli(ctx, ["sign", str(out_pkg), "--key", str(key_file)])
        assert p_sign.returncode == 0, p_sign.stderr
        sig_path = out_pkg.with_suffix(".cmplugin.minisig")
        assert sig_path.is_file(), f"Missing generated .minisig at {sig_path}"
        sig_text = sig_path.read_text(encoding="utf-8")
        digest = hashlib.sha256(out_pkg.read_bytes()).digest()
        ver = verify_minisign(digest, sig_text, kp.public_key_text)
        assert ver.valid, f"Minisign CLI signature invalid: {ver.reason}"
        set_developer_mode(ctx, True)
        try:
            p_dev = run_plugin_cli(ctx, ["dev", str(proj), "--instance", ctx.base_url])
            assert p_dev.returncode == 0, p_dev.stderr
        finally:
            set_developer_mode(ctx, False)

    # -----------------------------------------------------------------------
    # Feature #24: plugin_ui_trust, Descriptor, Trust & Cookie Asset Serving
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F24-01",
        tier=1,
        feature_id=24,
        name="Cookie-only GET /plugins/:id/ui returns UiDescriptor with subject_user_id and zero secrets",
        self_test=False,
    )
    def test_f24_cookie_descriptor(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f24d')}", kind="ui-only", enable=True
        )
        me = ctx.client.me()
        cookie_cli = ctx.client.clone_cookie_only()
        desc = cookie_cli.get(f"/api/v1/plugins/{pkg.plugin_id}/ui")
        assert desc["subject_user_id"] == me["id"]
        assert desc["contract"] == "1.0.0"
        assert desc["react"] == "19.3.0"
        assert desc["trust"]["status"] != "trusted"
        assert isinstance(desc["surfaces"], list) and len(desc["surfaces"]) >= 1

    @registry.case(
        id="T1-F24-02",
        tier=1,
        feature_id=24,
        name="UI trust prepare -> reauth -> grant enables asset access and /plugin-ui-navigation",
        self_test=False,
    )
    def test_f24_trust_grant_and_navigation(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f24nav')}",
            kind="ui-only",
            ui_overrides={
                "surfaces": [
                    {"id": "app", "kind": "app", "expose": "./App", "title": "My App", "icon": "puzzle"},
                    {"id": "dash", "kind": "dashboard", "expose": "./Dashboard", "title": "My Dash", "icon": "gauge"},
                ]
            },
            enable=True,
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/ui")
        assert desc["trust"]["status"] == "trusted"
        nav = ctx.call_phase1("GET", "/api/v1/plugin-ui-navigation")
        assert any(item.get("plugin_id") == pkg.plugin_id for item in nav)

    @registry.case(
        id="T1-F24-03",
        tier=1,
        feature_id=24,
        name="GET and HEAD /plugin-ui-assets/:id/:hash/*path enforce cookie, MIME, nosniff, and no-store",
        self_test=False,
    )
    def test_f24_cookie_asset_headers(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f24ast')}", kind="ui-only", enable=True
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        cookie_cli = ctx.client.clone_cookie_only()
        asset_path = f"/api/v1/plugin-ui-assets/{pkg.plugin_id}/{pkg.sha256_hex}/ui/assets/remoteEntry.js"
        get_r = cookie_cli.get_asset(asset_path, use_cookie=True, no_bearer=True)
        head_r = cookie_cli.head_asset(asset_path, use_cookie=True, no_bearer=True)
        assert get_r.status == 200 and head_r.status == 200
        assert "javascript" in (get_r.header("content-type") or "").lower()
        assert get_r.header("x-content-type-options") == "nosniff"
        cc = (get_r.header("cache-control") or "").lower()
        assert "private" in cc and "no-store" in cc
        with zipfile.ZipFile(io.BytesIO(pkg.bytes), "r") as zf:
            expected_entry = zf.read("ui/assets/remoteEntry.js")
        assert len(get_r.body) > 0 and get_r.body == expected_entry, (
            f"Expected uploaded remoteEntry.js bytes ({len(expected_entry)}B), got {get_r.body!r}"
        )

    @registry.case(
        id="T1-F24-04",
        tier=1,
        feature_id=24,
        name="POST /plugins/:id/ui/query returns bounded sanitized OptionViews within capabilities",
        self_test=False,
    )
    def test_f24_ui_query(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=supersecret")
        site = ensure_site(ctx, pt.url, profile_id="pterclub", cookie="uid=42; pass=supersecret")
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f24q')}",
            kind="ui-only",
            ui_overrides={"capabilities": ["read:sites", "read:downloaders", "read:libraries", "read:catalog"]},
            enable=True,
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        res = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/ui/query",
            json_body={"name": "sites.list", "input": {"limit": 50}},
        )
        assert "supersecret" not in str(res)
        items = res.get("items") if isinstance(res, dict) else res
        assert isinstance(items, list) and len(items) >= 1, (
            f"Expected non-empty sites.list result, got {res!r}"
        )
        assert any(site["id"] in str(it) or site["name"] in str(it) for it in items), (
            f"Expected site {site['id']}/{site['name']} in sites.list {items!r}"
        )

    @registry.case(
        id="T1-F24-05",
        tier=1,
        feature_id=24,
        name="POST /plugins/:id/ui-trust/revoke blocks asset fetches while backend plugin remains enabled",
        self_test=False,
    )
    def test_f24_trust_revoke_keeps_backend_enabled(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f24rev')}", kind="ui-only", enable=True
        )
        granted = grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        rev = str(granted.get("revision", 1))
        ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/ui-trust/revoke",
            json_body={"package_hash": pkg.sha256_hex},
            if_match=rev,
            idempotency_key=uniq("uirev"),
        )
        cookie_cli = ctx.client.clone_cookie_only()
        r = cookie_cli.get_asset(
            f"/api/v1/plugin-ui-assets/{pkg.plugin_id}/{pkg.sha256_hex}/ui/assets/remoteEntry.js"
        )
        assert r.status == 403
        plugin_after = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}")
        assert plugin_after["lifecycle"] == "enabled"

    # -----------------------------------------------------------------------
    # Feature #25: Frontend Shell (/plugin-apps/:id/:surfaceId), 4 Surfaces, SDK
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F25-01",
        tier=1,
        feature_id=25,
        name="/plugin-apps/:id/:surfaceId returns 200 HTML while /plugins/:id/:surfaceId returns 404 JSON",
        self_test=False,
    )
    def test_f25_route_separation(ctx: TestContext) -> None:
        # Ensure plugin router is present first
        ctx.call_phase1("GET", "/api/v1/plugins")
        jf = ctx.client.raw_request("GET", "/plugins/org.example.demo/checkin", no_bearer=True, no_cookie=True)
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
                c = ui_sup.client()
                app_r = c.raw_request("GET", "/plugin-apps/org.example.demo/checkin", no_bearer=True, no_cookie=True)
                assert app_r.status == 200
                assert "text/html" in (app_r.header("content-type") or "").lower()
                html_text = app_r.body.decode("utf-8", errors="ignore").lower()
                assert (
                    '<div id="root"' in html_text
                    or "<div id='root'" in html_text
                    or "<script" in html_text
                ), f"Expected SPA HTML shell markers in /plugin-apps response, got {html_text[:200]!r}"
        else:
            app_r = ctx.client.raw_request("GET", "/plugin-apps/org.example.demo/checkin", no_bearer=True, no_cookie=True)
            assert app_r.status == 200
            assert "text/html" in (app_r.header("content-type") or "").lower()
            html_text = app_r.body.decode("utf-8", errors="ignore").lower()
            assert (
                '<div id="root"' in html_text
                or "<div id='root'" in html_text
                or "<script" in html_text
            ), f"Expected SPA HTML shell markers in /plugin-apps response, got {html_text[:200]!r}"

    @registry.case(
        id="T1-F25-02",
        tier=1,
        feature_id=25,
        name="/auth/me and /auth/login user.id match UiDescriptor.subject_user_id",
        self_test=False,
    )
    def test_f25_subject_identity_match(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx, plugin_id=f"org.example.{uniq('f25id')}", kind="ui-only", enable=True
        )
        me = ctx.client.me()
        assert me.get("id")
        desc = ctx.client.clone_cookie_only().get(f"/api/v1/plugins/{pkg.plugin_id}/ui")
        assert desc["subject_user_id"] == me["id"]

    @registry.case(
        id="T1-F25-03",
        tier=1,
        feature_id=25,
        name="All 4 surface kinds (config, page, app, dashboard) appear in UiDescriptor and navigation",
        self_test=False,
    )
    def test_f25_four_surfaces(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f25s4')}",
            kind="ui-only",
            ui_overrides={
                "surfaces": [
                    {"id": "cfg", "kind": "config", "expose": "./Config", "title": "Config", "icon": "sliders"},
                    {"id": "pg", "kind": "page", "expose": "./Page", "title": "Page", "icon": "file-text"},
                    {"id": "app", "kind": "app", "expose": "./App", "title": "App", "icon": "puzzle"},
                    {"id": "dash", "kind": "dashboard", "expose": "./Dashboard", "title": "Dash", "icon": "activity"},
                ]
            },
            enable=True,
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/ui")
        kinds = {s["kind"]: s for s in desc["surfaces"]}
        assert set(kinds.keys()) == {"config", "page", "app", "dashboard"}
        assert all(s["mountable"] is True for s in desc["surfaces"])
        nav = ctx.call_phase1("GET", "/api/v1/plugin-ui-navigation")
        assert any(n.get("plugin_id") == pkg.plugin_id for n in nav)

    @registry.case(
        id="T1-F25-04",
        tier=1,
        feature_id=25,
        name="Config surface is editable when disabled and redacts secrets on GET/PUT /config",
        self_test=False,
    )
    def test_f25_config_surface_and_secrets(ctx: TestContext) -> None:
        pkg, _, _ = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f25cfg')}",
            kind="ui-only",
            schema_override=build_secret_config_schema(),
            enable=False,
        )
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/ui")
        cfg_surf = [s for s in desc["surfaces"] if s["kind"] == "config"][0]
        assert cfg_surf["editable"] is True
        cfg0 = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}/config")
        updated = ctx.call_phase1(
            "PUT",
            f"/api/v1/plugins/{pkg.plugin_id}/config",
            json_body={
                "schema_revision": cfg0["schema_revision"],
                "values": {"profile_label": "s1", "dry_run": False, "threshold": 50},
                "secrets": {"api_token": {"action": "replace", "value": "ui-secret-123"}},
            },
            if_match=str(cfg0["revision"]),
            idempotency_key=uniq("f25cfg"),
        )
        assert updated["secrets"]["api_token"]["configured"] is True
        assert "ui-secret-123" not in str(updated)

    @registry.case(
        id="T1-F25-05",
        tier=1,
        feature_id=25,
        name="Host SDK opaque handle resolution and command submission round-trip",
        self_test=False,
    )
    def test_f25_opaque_handle_roundtrip(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        pkg, _, plugin = setup_signed_plugin_ready(
            ctx,
            plugin_id=f"org.example.{uniq('f25sdk')}",
            kind="ui-only",
            ui_overrides={"capabilities": ["read:sites", "read:catalog", "command:indexer.search"]},
            enable=True,
        )
        grant_ui_trust(ctx, pkg.plugin_id, pkg.sha256_hex)
        resolved = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/ui/query",
            json_body={"name": "handles.resolve", "input": {"kind": "site", "id": site["id"]}},
        )
        assert isinstance(resolved, dict) and resolved.get("handle"), (
            f"Expected opaque handle in resolved response: {resolved!r}"
        )
        cmd_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pkg.plugin_id}/commands",
            json_body={
                "kind": "indexer.search",
                "payload": {"site_handle": resolved["handle"], "keyword": "E2E"},
            },
            plugin_package=pkg.sha256_hex,
            plugin_generation=str(plugin["generation"]),
            idempotency_key=uniq("f25cmd"),
        )
        assert cmd_op.get("operation_id")

    # -----------------------------------------------------------------------
    # Feature #26: First-Party Check-in & Boost React UIs
    # -----------------------------------------------------------------------

    @registry.case(
        id="T1-F26-01",
        tier=1,
        feature_id=26,
        name="Legacy boost routes return 404 while plugin routes are active",
        self_test=False,
    )
    def test_f26_legacy_boost_deleted(ctx: TestContext) -> None:
        ctx.call_phase1("GET", "/api/v1/plugins")
        r_stats = ctx.client.raw_request("GET", "/api/v1/sites/boost-stats")
        r_patch = ctx.client.raw_request(
            "PATCH",
            "/api/v1/sites/00000000-0000-0000-0000-000000000000/boost",
            body=b"{}",
            headers={"Content-Type": "application/json"},
        )
        assert r_stats.status == 404, f"Legacy /sites/boost-stats must be 404, got {r_stats.status}"
        assert r_patch.status == 404, f"Legacy PATCH /sites/:id/boost must be 404, got {r_patch.status}"

    @registry.case(
        id="T1-F26-02",
        tier=1,
        feature_id=26,
        name="First-party Check-in plugin exposes 4 UI surfaces and serves remoteEntry.js via cookie",
        self_test=False,
    )
    def test_f26_checkin_ui_discovery(ctx: TestContext) -> None:
        p = _find_first_party_plugin(ctx, ("builtin.checkin", "org.example.checkin"), "checkin")
        pid = p["id"]
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
        kinds = {s["kind"] for s in desc["surfaces"]}
        assert {"config", "page", "app", "dashboard"}.issubset(kinds)
        grant_ui_trust(ctx, pid, desc["package_hash"])
        asset_r = ctx.client.clone_cookie_only().get_asset(desc["entry_url"])
        assert asset_r.status == 200
        assert len(asset_r.body) > 0, "Expected non-empty remoteEntry.js body"
        ct = (asset_r.header("content-type") or "").lower()
        assert "javascript" in ct, f"Expected javascript Content-Type, got {ct!r}"
        assert asset_r.header("x-content-type-options") == "nosniff", (
            f"Expected X-Content-Type-Options: nosniff, got {asset_r.headers!r}"
        )

    @registry.case(
        id="T1-F26-03",
        tier=1,
        feature_id=26,
        name="Check-in UI query checkin.receipts is read-only; explicit command executes check-in once",
        self_test=False,
    )
    def test_f26_checkin_ui_readonly_then_run(ctx: TestContext) -> None:
        pt = ctx.new_pt_site(expected_cookie="uid=42; pass=valid")
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        p = _find_first_party_plugin(ctx, ("builtin.checkin", "org.example.checkin"), "checkin")
        pid = p["id"]
        if p.get("lifecycle") != "enabled":
            enable_plugin(ctx, pid, revision=int(p["revision"]))
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
        grant_ui_trust(ctx, pid, desc["package_hash"])
        ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/ui/query",
            json_body={"name": "checkin.receipts", "input": {"site_id": site["id"]}},
        )
        assert pt.checkin_post_count == 0
        op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/commands",
            json_body={"kind": "site.maintenance", "payload": {"site_id": site["id"], "action": "check-in"}},
            idempotency_key=uniq("chkcmd"),
        )
        assert poll_operation(ctx, op["operation_id"])["state"] == "succeeded"
        assert pt.checkin_post_count == 1

    @registry.case(
        id="T1-F26-04",
        tier=1,
        feature_id=26,
        name="First-party Boost plugin exposes 4 UI surfaces and consistent boost.snapshot/brush-state",
        self_test=False,
    )
    def test_f26_boost_ui_and_snapshot(ctx: TestContext) -> None:
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit()
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        p = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        pid = p["id"]
        if p.get("lifecycle") != "enabled":
            enable_plugin(ctx, pid, revision=int(p["revision"]))
        desc = ctx.call_phase1("GET", f"/api/v1/plugins/{pid}/ui")
        assert {"config", "page", "app", "dashboard"}.issubset({s["kind"] for s in desc["surfaces"]})
        grant_ui_trust(ctx, pid, desc["package_hash"])
        snap = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/ui/query",
            json_body={"name": "boost.snapshot", "input": {"site_id": site["id"], "downloader_id": dl["id"]}},
        )
        state = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{pid}/brush-state?site_id={site['id']}&downloader_id={dl['id']}",
        )
        assert isinstance(snap, dict) and "used_bytes" in snap and "retained_file_bytes" in snap, (
            f"Expected used_bytes and retained_file_bytes in boost.snapshot, got {snap!r}"
        )
        assert isinstance(state, dict) and "used_bytes" in state and "retained_file_bytes" in state, (
            f"Expected used_bytes and retained_file_bytes in brush-state, got {state!r}"
        )
        assert snap["used_bytes"] == state["used_bytes"]
        assert snap["retained_file_bytes"] == state["retained_file_bytes"]

    @registry.case(
        id="T1-F26-05",
        tier=1,
        feature_id=26,
        name="Boost brush-cleanup-preview enforces selection vs plan_handle and issues confirmation_token",
        self_test=False,
    )
    def test_f26_boost_cleanup_preview_flow(ctx: TestContext) -> None:
        save_dir = ctx.tmp_dir / "qb_f26p"
        pt = ctx.new_pt_site()
        qb = ctx.new_qbit(real_save_dir=save_dir)
        dl = ensure_downloader(ctx, qb.url)
        site = ensure_site(ctx, pt.url, profile_id="pterclub")
        p = _find_first_party_plugin(ctx, ("builtin.boost", "org.example.boost"), "boost")
        pid = p["id"]
        ensure_plugin_enabled(ctx, pid)
        add_op = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/commands",
            json_body={
                "kind": "downloader.add",
                "payload": {
                    "downloader_id": dl["id"],
                    "site_id": site["id"],
                    "download_url": f"{pt.url}/download.torrent",
                    "title": "F26 Preview Target",
                },
            },
            idempotency_key=uniq("f26add"),
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
            idempotency_key=uniq("f26del"),
        )
        assert poll_operation(ctx, del_op["operation_id"])["state"] == "succeeded"
        sel = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{pid}/brush-cleanup-preview?task_ids={tid}",
        )
        assert sel.get("mode") == "selection"
        assert sel.get("confirmation_token") is None
        prep = ctx.call_phase1(
            "POST",
            f"/api/v1/plugins/{pid}/commands",
            json_body={
                "kind": "downloader.prepare-cleanup",
                "payload": {"site_id": site["id"], "downloader_id": dl["id"], "task_ids": [tid]},
            },
            idempotency_key=uniq("f26prep"),
        )
        done = poll_operation(ctx, prep["operation_id"])
        assert done["state"] == "succeeded"
        plan_handle = (done.get("result") or {}).get("plan_handle")
        assert plan_handle, f"Missing plan_handle from prepare-cleanup: {done!r}"
        plan_prev = ctx.call_phase1(
            "GET",
            f"/api/v1/plugins/{pid}/brush-cleanup-preview?plan_handle={plan_handle}",
        )
        assert plan_prev.get("mode") == "plan"
        assert plan_prev.get("confirmation_token")
