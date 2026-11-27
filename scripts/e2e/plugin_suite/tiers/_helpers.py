"""Shared opaque-box helpers for Tier 1 and Tier 2 E2E test modules."""
from __future__ import annotations

from pathlib import Path
import subprocess
import threading
import time
from typing import Any, Callable, Dict, List, Optional, Sequence, Tuple
import uuid

from ..lib.client import ApiClient, ApiError
from ..lib.fixtures import (
    CmpluginPackage,
    MinisignKeypair,
    build_cmplugin,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
)
from ..lib.instance import InstanceSupervisor, default_binary_path
from . import TestContext, UnimplementedFeatureError


def uniq(prefix: str = "e2e") -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def post_multipart_phase1(
    client: ApiClient,
    path: str,
    fields: Optional[Dict[str, str]] = None,
    files: Optional[List[Tuple[str, str, bytes, str]]] = None,
    headers: Optional[Dict[str, str]] = None,
    if_match: Optional[str] = None,
    idempotency_key: Optional[str] = None,
    reauth_token: Optional[str] = None,
) -> Any:
    try:
        return client.post_multipart(
            path,
            fields=fields,
            files=files,
            headers=headers,
            if_match=if_match,
            idempotency_key=idempotency_key,
            reauth_token=reauth_token,
            expect_envelope=True,
        )
    except ApiError as exc:
        if exc.status in (404, 405, 501):
            raise UnimplementedFeatureError(f"POST {path}", f"HTTP {exc.status}") from exc
        raise


def assert_api_error(
    fn: Callable[[], Any],
    expected_statuses: Tuple[int, ...],
    label: str = "API call",
) -> ApiError:
    try:
        fn()
    except UnimplementedFeatureError as unimpl:
        cause = unimpl.__cause__
        if (
            isinstance(cause, ApiError)
            and cause.status in expected_statuses
            and cause.code is not None
            and cause.code != "not_found"
        ):
            return cause
        raise
    except ApiError as exc:
        if exc.status in (404, 405, 501) and (
            exc.status not in expected_statuses
            or exc.code is None
            or exc.code == "not_found"
        ):
            raise UnimplementedFeatureError(label, f"HTTP {exc.status}") from exc
        assert exc.status in expected_statuses, (
            f"{label}: expected HTTP {expected_statuses}, got {exc.status} ({exc.code}: {exc.message})"
        )
        return exc
    raise AssertionError(f"{label}: expected ApiError with status in {expected_statuses}, but succeeded")


def obtain_reauth(
    ctx: TestContext,
    purpose: str = "plugin:upload",
    client: Optional[ApiClient] = None,
    password: Optional[str] = None,
) -> str:
    cli = client or ctx.client
    pw = ctx.admin_password if password is None else password
    try:
        data = cli.post(
            "/api/v1/plugin-reauth",
            json_body={"password": pw, "purpose": purpose},
        )
    except ApiError as exc:
        if exc.status in (404, 405, 501):
            raise UnimplementedFeatureError("POST /api/v1/plugin-reauth", f"HTTP {exc.status}") from exc
        raise
    assert isinstance(data, dict) and "reauth_token" in data, (
        f"Expected dict with 'reauth_token' from /plugin-reauth, got {data!r}"
    )
    tok = data["reauth_token"]
    assert tok, f"Missing reauth_token from /plugin-reauth: {data!r}"
    return str(tok)


def set_developer_mode(ctx: TestContext, enabled: bool) -> Dict[str, Any]:
    settings = ctx.call_phase1("GET", "/api/v1/settings/plugins")
    rev = str(settings.get("revision", 0))
    current = settings["developer_mode"] if "developer_mode" in settings else settings.get("enabled")
    if bool(current) == enabled:
        return settings
    if enabled:
        reauth = obtain_reauth(ctx, purpose="developer-mode:enable")
        body: Dict[str, Any] = {"reauth_token": reauth}
        action = "enable"
    else:
        body = {}
        action = "disable"
    res = ctx.call_phase1(
        "POST",
        f"/api/v1/settings/plugins/developer/{action}",
        json_body=body,
        if_match=rev,
        idempotency_key=uniq("devmode"),
    )
    if isinstance(res, dict) and res.get("operation_id"):
        done = poll_operation(ctx, str(res["operation_id"]))
        assert done.get("state") == "succeeded", (
            f"Developer mode {action} failed: {done!r}"
        )
        return ctx.call_phase1("GET", "/api/v1/settings/plugins")
    return res


def pin_trust_key(
    ctx: TestContext,
    kp: MinisignKeypair,
    label: str = "e2e-publisher",
) -> Dict[str, Any]:
    prep = ctx.call_phase1(
        "POST",
        "/api/v1/plugin-trust-keys/prepare",
        json_body={"public_key": kp.public_key_text, "label": label},
    )
    reauth = obtain_reauth(ctx, purpose="trust-key:create")
    return ctx.call_phase1(
        "POST",
        "/api/v1/plugin-trust-keys",
        json_body={
            "public_key": kp.public_key_text,
            "fingerprint": prep["fingerprint"],
            "consent_token": prep["consent_token"],
            "reauth_token": reauth,
            "label": label,
        },
        if_match="0",
        idempotency_key=uniq("trust-pin"),
    )


def upload_cmplugin(
    ctx: TestContext,
    pkg_bytes: bytes,
    sig_text: Optional[str] = None,
    upload_id: Optional[str] = None,
    reauth_token: Optional[str] = None,
    idempotency_key: Optional[str] = None,
) -> Dict[str, Any]:
    uid = upload_id or str(uuid.uuid4())
    tok = reauth_token if reauth_token is not None else obtain_reauth(ctx, purpose="plugin:upload")
    files: List[Tuple[str, str, bytes, str]] = [
        ("file", "plugin.cmplugin", pkg_bytes, "application/zip")
    ]
    if sig_text is not None:
        files.append(("signature", "plugin.cmplugin.minisig", sig_text.encode("utf-8"), "text/plain"))
    return post_multipart_phase1(
        ctx.client,
        "/api/v1/plugin-uploads",
        fields={"upload_id": uid},
        files=files,
        reauth_token=tok,
        idempotency_key=idempotency_key or f"idem-{uid}",
        if_match="0",
    )


def poll_operation(
    ctx: TestContext,
    op_id: str,
    timeout: float = 15.0,
    terminal_states: Tuple[str, ...] = ("succeeded", "failed", "cancelled", "rejected"),
    client: Optional[ApiClient] = None,
) -> Dict[str, Any]:
    cli = client or ctx.client
    deadline = time.monotonic() + timeout
    last_op: Optional[Dict[str, Any]] = None
    while time.monotonic() < deadline:
        try:
            op = cli.get(f"/api/v1/plugin-operations/{op_id}")
        except ApiError as exc:
            if exc.status in (404, 405, 501):
                raise UnimplementedFeatureError(
                    f"GET /api/v1/plugin-operations/{op_id}", f"HTTP {exc.status}"
                ) from exc
            raise
        assert isinstance(op, dict), f"Expected operation dict for {op_id}, got {op!r}"
        last_op = op
        if op.get("state") in terminal_states:
            return op
        time.sleep(0.1)
    raise AssertionError(f"Operation {op_id} did not reach {terminal_states} within {timeout}s; last={last_op!r}")


def extract_prep_grants(prep: Dict[str, Any]) -> List[Any]:
    gd = prep.get("grant_diff")
    if isinstance(gd, dict):
        added = gd.get("added") or []
        kept = gd.get("kept") or []
        combined = list(added) + list(kept)
        if combined:
            return combined
        items = gd.get("requested")
        if not items:
            items = gd.get("grants")
        if items:
            return list(items)
    if prep.get("grants"):
        return list(prep["grants"])
    return []


def prepare_and_install_plugin(
    ctx: TestContext,
    pkg: CmpluginPackage,
    kp: Optional[MinisignKeypair] = None,
    sig_text: Optional[str] = None,
    grants: Optional[List[Any]] = None,
    pin_key: bool = True,
) -> Tuple[Dict[str, Any], str]:
    if kp is not None and pin_key:
        pin_trust_key(ctx, kp, label=f"key-{uniq()}")
    if sig_text is None and kp is not None:
        sig_text = sign_cmplugin_sha256(pkg.bytes, kp, hashed=True)
    uploaded = upload_cmplugin(ctx, pkg.bytes, sig_text=sig_text)
    assert isinstance(uploaded, dict) and "hash" in uploaded and "upload_id" in uploaded, (
        f"Expected upload dict with 'hash' and 'upload_id', got {uploaded!r}"
    )
    assert uploaded["hash"] == pkg.sha256_hex, (
        f"Expected uploaded hash {pkg.sha256_hex}, got {uploaded!r}"
    )
    prep_op = ctx.call_phase1(
        "POST",
        "/api/v1/plugin-installations/prepare",
        json_body={"upload_id": uploaded["upload_id"]},
        if_match="0",
        idempotency_key=uniq("prep"),
    )
    assert isinstance(prep_op, dict) and "operation_id" in prep_op, (
        f"Expected operation dict with 'operation_id' from prepare, got {prep_op!r}"
    )
    prep_done = poll_operation(ctx, prep_op["operation_id"])
    assert isinstance(prep_done, dict) and prep_done.get("state") == "succeeded", (
        f"Install prepare failed: {prep_done!r}"
    )
    prep = prep_done.get("result") or prep_done
    assert isinstance(prep, dict) and prep.get("package_hash") == pkg.sha256_hex, (
        f"Expected prep package_hash {pkg.sha256_hex}, got {prep!r}"
    )
    assert "preparation_id" in prep and "consent_token" in prep, (
        f"Expected preparation_id and consent_token in prep: {prep!r}"
    )
    eff_grants = grants if grants is not None else extract_prep_grants(prep)
    inst_op = ctx.call_phase1(
        "POST",
        "/api/v1/plugin-installations",
        json_body={
            "preparation_id": prep["preparation_id"],
            "consent_token": prep["consent_token"],
            "grants": eff_grants,
        },
        if_match="0",
        idempotency_key=uniq("inst"),
    )
    assert isinstance(inst_op, dict) and "operation_id" in inst_op, (
        f"Expected operation dict with 'operation_id' from install, got {inst_op!r}"
    )
    inst_done = poll_operation(ctx, inst_op["operation_id"])
    assert isinstance(inst_done, dict) and inst_done.get("state") == "succeeded", (
        f"Install failed: {inst_done!r}"
    )
    plugin = ctx.call_phase1("GET", f"/api/v1/plugins/{pkg.plugin_id}")
    assert isinstance(plugin, dict) and "revision" in plugin, (
        f"Expected plugin object with 'revision' for {pkg.plugin_id}, got {plugin!r}"
    )
    assert plugin.get("id") == pkg.plugin_id, (
        f"Expected plugin id {pkg.plugin_id}, got {plugin!r}"
    )
    assert plugin.get("lifecycle") == "installed-disabled", (
        f"Expected installed-disabled lifecycle after install of {pkg.plugin_id}, got {plugin!r}"
    )
    if pkg.plugin_id not in ctx.created_plugin_ids:
        ctx.created_plugin_ids.append(pkg.plugin_id)
    return plugin, uploaded["upload_id"]


def enable_plugin(ctx: TestContext, plugin_id: str, revision: Optional[int] = None) -> Dict[str, Any]:
    if revision is None:
        plugin = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}")
        assert isinstance(plugin, dict) and "revision" in plugin, (
            f"Expected plugin object with 'revision' for {plugin_id}, got {plugin!r}"
        )
        revision = int(plugin["revision"])
    op = ctx.call_phase1(
        "POST",
        f"/api/v1/plugins/{plugin_id}/enable",
        json_body={},
        if_match=str(revision),
        idempotency_key=uniq("enable"),
    )
    assert isinstance(op, dict) and "operation_id" in op, (
        f"Expected operation dict with 'operation_id' from enable {plugin_id}, got {op!r}"
    )
    done = poll_operation(ctx, op["operation_id"])
    assert isinstance(done, dict) and done.get("state") == "succeeded", (
        f"Enable {plugin_id} failed: {done!r}"
    )
    enabled = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}")
    assert isinstance(enabled, dict) and "lifecycle" in enabled and "generation" in enabled, (
        f"Expected plugin dict with 'lifecycle' and 'generation' for {plugin_id}, got {enabled!r}"
    )
    assert enabled["lifecycle"] == "enabled", (
        f"Expected enabled lifecycle for {plugin_id}, got {enabled!r}"
    )
    assert int(enabled["generation"]) >= 1, (
        f"Expected generation >= 1 after enabling {plugin_id}, got {enabled!r}"
    )
    return enabled


def disable_plugin(ctx: TestContext, plugin_id: str, reason: str = "e2e-disable") -> Dict[str, Any]:
    plugin = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}")
    assert isinstance(plugin, dict) and "revision" in plugin, (
        f"Expected plugin object with 'revision' for {plugin_id}, got {plugin!r}"
    )
    rev = int(plugin["revision"])
    op = ctx.call_phase1(
        "POST",
        f"/api/v1/plugins/{plugin_id}/disable",
        json_body={"reason": reason},
        if_match=str(rev),
        idempotency_key=uniq("disable"),
    )
    assert isinstance(op, dict) and "operation_id" in op, (
        f"Expected operation dict with 'operation_id' from disable {plugin_id}, got {op!r}"
    )
    done = poll_operation(ctx, op["operation_id"])
    assert isinstance(done, dict) and done.get("state") == "succeeded", (
        f"Disable {plugin_id} failed: {done!r}"
    )
    disabled = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}")
    assert isinstance(disabled, dict) and "lifecycle" in disabled, (
        f"Expected plugin dict with 'lifecycle' after disable {plugin_id}, got {disabled!r}"
    )
    return disabled


def setup_signed_plugin_ready(
    ctx: TestContext,
    plugin_id: Optional[str] = None,
    version: str = "1.0.0",
    kind: str = "hook",
    include_ui: bool = False,
    ui_overrides: Optional[Dict[str, Any]] = None,
    schema_override: Optional[Dict[str, Any]] = None,
    grants: Optional[List[Any]] = None,
    enable: bool = True,
    seed: Optional[bytes] = None,
    tracked_pids: Optional[List[str]] = None,
    **pkg_kwargs: Any,
) -> Tuple[CmpluginPackage, MinisignKeypair, Dict[str, Any]]:
    pid = plugin_id or f"org.example.{uniq('p')}"
    if tracked_pids is not None:
        tracked_pids.append(pid)
    kp = generate_minisign_keypair(seed=seed or uuid.uuid4().bytes + uuid.uuid4().bytes)
    pkg = build_cmplugin(
        plugin_id=pid,
        version=version,
        kind=kind,
        include_ui=include_ui,
        ui_overrides=ui_overrides,
        schema_override=schema_override,
        **pkg_kwargs,
    )
    plugin, _ = prepare_and_install_plugin(ctx, pkg, kp=kp, grants=grants)
    if enable:
        plugin = enable_plugin(ctx, pid, revision=int(plugin["revision"]))
    return pkg, kp, plugin


def grant_ui_trust(
    ctx: TestContext,
    plugin_id: str,
    package_hash: str,
    client: Optional[ApiClient] = None,
    password: Optional[str] = None,
) -> Dict[str, Any]:
    cli = client or ctx.client
    try:
        prep = cli.post(
            f"/api/v1/plugins/{plugin_id}/ui-trust/prepare",
            json_body={"package_hash": package_hash},
        )
    except ApiError as exc:
        if exc.status in (404, 405, 501):
            raise UnimplementedFeatureError(
                f"POST /api/v1/plugins/{plugin_id}/ui-trust/prepare", f"HTTP {exc.status}"
            ) from exc
        raise
    reauth = obtain_reauth(ctx, purpose="plugin-ui-trust", client=cli, password=password)
    rev = str((prep.get("trust_revision") if isinstance(prep, dict) else None) or 0)
    return cli.post(
        f"/api/v1/plugins/{plugin_id}/ui-trust/grant",
        json_body={
            "package_hash": package_hash,
            "consent_token": prep["consent_token"],
            "reauth_token": reauth,
        },
        if_match=rev,
        idempotency_key=uniq("ui-trust"),
    )


def create_member_client(ctx: TestContext) -> Tuple[str, ApiClient]:
    login_name = uniq("member").replace("-", "_")
    pw = f"pw-{uuid.uuid4().hex[:12]}"
    created = ctx.client.create_member(login_name, pw)
    mc = ctx.new_client()
    mc.login(login_name, pw)
    return str(created["id"]), mc


def make_temp_web_dir(ctx: TestContext) -> Path:
    web_dir = ctx.tmp_dir / "web_dist"
    web_dir.mkdir(parents=True, exist_ok=True)
    (web_dir / "index.html").write_text(
        "<!doctype html><html><body><div id='root'></div></body></html>\n",
        encoding="utf-8",
    )
    return web_dir


def resolve_bin_path(ctx: TestContext) -> Path:
    if ctx.bin_path is not None:
        return Path(ctx.bin_path).resolve()
    if ctx.supervisor is not None:
        return ctx.supervisor.bin_path
    return default_binary_path().resolve()


def run_plugin_cli(
    ctx: TestContext,
    args: Sequence[str],
    cwd: Optional[Path] = None,
    env: Optional[Dict[str, str]] = None,
    timeout: float = 30.0,
) -> subprocess.CompletedProcess:
    bin_path = resolve_bin_path(ctx)
    if not bin_path.is_file():
        raise UnimplementedFeatureError(
            f"CLI: {bin_path} plugin {' '.join(args)}",
            "binary not found",
        )
    if ctx.supervisor is not None:
        proc = ctx.supervisor.run_cli(["plugin"] + list(args), cwd=cwd, env=env, timeout=timeout)
    else:
        proc = subprocess.run(
            [str(bin_path), "plugin"] + list(args),
            cwd=str(cwd or ctx.tmp_dir),
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    if proc.returncode != 0:
        combined = f"{proc.stdout}\n{proc.stderr}".lower()
        if (
            "unrecognized subcommand 'plugin'" in combined
            or 'unrecognized subcommand "plugin"' in combined
            or "unknown subcommand 'plugin'" in combined
            or "unexpected argument 'plugin'" in combined
            or "error: unrecognized subcommand 'plugin'" in combined
            or 'error: unrecognized subcommand "plugin"' in combined
            or "unknown command plugin" in combined
            or "unknown command 'plugin'" in combined
            or 'unknown command "plugin"' in combined
        ):
            raise UnimplementedFeatureError(
                f"CLI: {bin_path.name} plugin", proc.stderr.strip()[:200]
            )
    return proc


def ensure_library(ctx: TestContext, kind: str = "movie", name: Optional[str] = None) -> Dict[str, Any]:
    lib_dir = ctx.tmp_dir / uniq("lib")
    lib_dir.mkdir(parents=True, exist_ok=True)
    return ctx.call_phase1(
        "POST",
        "/api/v1/libraries",
        json_body={
            "name": name or uniq("E2ELib"),
            "kind": kind,
            "root_paths": [str(lib_dir)],
        },
    )


def ensure_downloader(
    ctx: TestContext,
    qb_url: str,
    name: Optional[str] = None,
    is_default: bool = True,
) -> Dict[str, Any]:
    dl = ctx.call_phase1(
        "POST",
        "/api/v1/downloaders",
        json_body={
            "name": name or uniq("qb"),
            "kind": "qbittorrent",
            "url": qb_url,
            "username": "admin",
            "password": "adminadmin",
            "enabled": True,
            "is_default": is_default,
        },
    )
    if isinstance(dl, dict) and dl.get("id") and dl["id"] not in ctx.created_downloader_ids:
        ctx.created_downloader_ids.append(dl["id"])
    return dl


def ensure_site(
    ctx: TestContext,
    pt_url: str,
    profile_id: str = "demo",
    cookie: str = "uid=42; pass=valid",
    api_key: Optional[str] = None,
    name: Optional[str] = None,
) -> Dict[str, Any]:
    body: Dict[str, Any] = {
        "name": name or uniq("site"),
        "profile_id": profile_id,
        "url": pt_url,
        "enabled": True,
    }
    if api_key is not None:
        body["api_key"] = api_key
    else:
        body["cookie"] = cookie
    site = ctx.call_phase1("POST", "/api/v1/sites", json_body=body)
    if isinstance(site, dict) and site.get("id"):
        if site["id"] not in ctx.created_site_ids:
            ctx.created_site_ids.append(site["id"])
        if "revision" not in site:
            st = ctx.call_phase1("GET", f"/api/v1/sites/{site['id']}/plugin-status")
            if isinstance(st, dict) and "revision" in st:
                site["revision"] = st["revision"]
    return site


def require_restart_supervisor(ctx: TestContext) -> InstanceSupervisor:
    if ctx.supervisor is not None:
        return ctx.supervisor
    raise UnimplementedFeatureError(
        "InstanceSupervisor crash/restart",
        "requires --spawn-backend mode",
    )


def restart_and_relogin(ctx: TestContext, kill: bool = False) -> str:
    return ctx.restart_server(kill=kill, relogin=True)


def put_browser_settings(
    ctx: TestContext, enabled: bool, url: Optional[str] = None
) -> Dict[str, Any]:
    cdp_cfg: Dict[str, Any] = {"enabled": enabled}
    if url is not None:
        cdp_cfg["url"] = url
    res = ctx.call_phase1(
        "PUT",
        "/api/v1/settings/browser",
        json_body={"cdp": cdp_cfg},
    )
    assert isinstance(res, dict) and res.get("cdp", {}).get("enabled") is enabled, (
        f"Expected cdp.enabled={enabled}, got {res!r}"
    )
    return res


def grant_site_browser_permission(
    ctx: TestContext, plugin_id: str, site_id: str, capability: str
) -> None:
    perms = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}/permissions")
    if isinstance(perms, dict):
        granted = perms.get("effective_grants") or perms.get("grants") or []
    elif isinstance(perms, list):
        granted = perms
    else:
        granted = []
    for item in granted:
        if isinstance(item, str) and item == capability:
            return
        if isinstance(item, dict) and item.get("capability") == capability:
            scope = item.get("scope")
            if scope in ("Global", "global", None) or site_id in str(scope):
                return
    plugin = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}")
    was_enabled = plugin.get("lifecycle") == "enabled"
    if was_enabled:
        disable_plugin(ctx, plugin_id)
        plugin = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}")
    req_grants: List[Any] = [
        {"capability": capability, "scope": {"Site": site_id}, "sensitive": True}
    ]
    prep = ctx.call_phase1(
        "POST",
        f"/api/v1/plugins/{plugin_id}/permissions/prepare",
        json_body={"requested_grants": req_grants},
    )
    eff_grants = extract_prep_grants(prep) or req_grants
    gr = ctx.call_phase1(
        "POST",
        f"/api/v1/plugins/{plugin_id}/permissions/grant",
        json_body={
            "consent_token": prep["consent_token"],
            "grants": eff_grants,
        },
        if_match=str(plugin["revision"]),
        idempotency_key=uniq("grant-browser"),
    )
    if isinstance(gr, dict) and gr.get("operation_id"):
        assert poll_operation(ctx, gr["operation_id"])["state"] == "succeeded"
    if was_enabled:
        enable_plugin(ctx, plugin_id)


def create_subscribe(
    ctx: TestContext,
    lib_id: str,
    title: str = "E2E Demo Movie",
    kind: str = "movie",
) -> Dict[str, Any]:
    return ctx.call_phase1(
        "POST",
        "/api/v1/subscriptions",
        json_body={
            "media": {"kind": kind, "title": title},
            "library_id": lib_id,
        },
    )


def unwrap_items(resp: Any, label: str = "response") -> List[Any]:
    assert isinstance(resp, dict) and isinstance(resp.get("items"), list), (
        f"Expected {{'items': [...]}} for {label}, got {resp!r}"
    )
    return list(resp["items"])


def unwrap_list(resp: Any, label: str = "response") -> List[Any]:
    assert isinstance(resp, list), f"Expected list for {label}, got {resp!r}"
    return list(resp)


_TICK_CLOCK_LOCK = threading.Lock()
_LAST_TICK_NOW = 0


def _next_tick_now() -> int:
    global _LAST_TICK_NOW
    with _TICK_CLOCK_LOCK:
        candidate = int(time.time()) + 60
        if candidate <= _LAST_TICK_NOW:
            candidate = _LAST_TICK_NOW + 60
        _LAST_TICK_NOW = candidate
        return candidate


def trigger_job(
    ctx: TestContext,
    kind: str,
    max_ticks: int = 32,
    scope_key: Optional[str] = None,
) -> Dict[str, Any]:
    def _find_matching_defs() -> List[Dict[str, Any]]:
        jobs = ctx.call_phase1("GET", "/api/v1/jobs")
        if not isinstance(jobs, list):
            return []
        matched: List[Dict[str, Any]] = []
        for j in jobs:
            if not isinstance(j, dict):
                continue
            if j.get("kind") != kind:
                continue
            if scope_key is not None and j.get("scope_key") != scope_key:
                continue
            if j.get("enabled", True) is False:
                continue
            if j.get("id"):
                matched.append(j)
        return matched

    def _expedite_defs(defs: List[Dict[str, Any]]) -> None:
        for job_def in defs:
            ctx.call_phase1("POST", f"/api/v1/jobs/{job_def['id']}/run", json_body={})

    defs = _find_matching_defs()
    initial_finished: Dict[str, Any] = {
        str(d["id"]): d.get("last_finished_at") for d in defs
    }
    if defs:
        _expedite_defs(defs)

    tick_res: Any = None
    for attempt in range(max_ticks):
        now_ts = _next_tick_now()
        tick_res = ctx.call_phase1("POST", f"/api/v1/jobs/tick?now={now_ts}", json_body={})
        if isinstance(tick_res, dict):
            kinds = tick_res.get("kinds")
            if isinstance(kinds, list) and kind in kinds:
                if scope_key is None:
                    return tick_res
                latest = _find_matching_defs()
                if latest and any(
                    d.get("last_finished_at") is not None
                    and d.get("last_finished_at") != initial_finished.get(str(d["id"]))
                    for d in latest
                ):
                    return tick_res
        if attempt + 1 < max_ticks:
            ran = tick_res.get("ran") if isinstance(tick_res, dict) else None
            latest_defs = _find_matching_defs()
            busy = any(d.get("last_status") == "running" for d in latest_defs)
            if not busy and any(
                d.get("last_finished_at") is not None
                and d.get("last_finished_at") != initial_finished.get(str(d["id"]))
                for d in latest_defs
            ):
                return tick_res if isinstance(tick_res, dict) else {"ran": 1, "kinds": [kind]}
            if ran in (0, None) or busy:
                time.sleep(0.05)
            if latest_defs and not busy:
                _expedite_defs(latest_defs)
    raise AssertionError(
        f"trigger_job({kind!r}) did not execute within {max_ticks} ticks: last={tick_res!r}"
    )


def ensure_plugin_enabled(ctx: TestContext, plugin_id: str) -> Dict[str, Any]:
    plugin = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}")
    assert isinstance(plugin, dict) and "revision" in plugin, (
        f"Expected plugin object with 'revision' for {plugin_id}, got {plugin!r}"
    )
    if plugin_id == "builtin.boost":
        cfg = ctx.call_phase1("GET", "/api/v1/plugins/builtin.boost/config")
        vals = dict(cfg.get("values") or {})
        if int(vals.get("max_adds_per_hour", 10)) < 100:
            if plugin.get("lifecycle") == "enabled":
                plugin = disable_plugin(ctx, plugin_id, reason="raise-e2e-add-limit")
            vals["max_adds_per_hour"] = 100
            ctx.call_phase1(
                "PUT",
                "/api/v1/plugins/builtin.boost/config",
                json_body={
                    "schema_revision": cfg["schema_revision"],
                    "values": vals,
                    "secrets": {},
                },
                if_match=str(plugin["revision"]),
                idempotency_key=uniq("boostcfg"),
            )
            plugin = ctx.call_phase1("GET", f"/api/v1/plugins/{plugin_id}")
    if plugin.get("lifecycle") != "enabled":
        return enable_plugin(ctx, plugin_id, revision=int(plugin["revision"]))
    return plugin


def poll_scan_operation(
    ctx: TestContext,
    lib_id: str,
    op_id: str,
    timeout: float = 15.0,
) -> Dict[str, Any]:
    deadline = time.monotonic() + timeout
    last_op: Optional[Dict[str, Any]] = None
    while time.monotonic() < deadline:
        op = ctx.call_phase1(
            "GET", f"/api/v1/libraries/{lib_id}/scan-operations/{op_id}"
        )
        last_op = op
        if op.get("status") in ("completed", "partial", "cancelled", "failed"):
            return op
        time.sleep(0.1)
    raise AssertionError(
        f"Scan operation {op_id} did not finish within {timeout}s; last={last_op!r}"
    )


def poll_library_operation(
    ctx: TestContext,
    lib_id: str,
    op_id: str,
    timeout: float = 15.0,
) -> Dict[str, Any]:
    deadline = time.monotonic() + timeout
    last_op: Optional[Dict[str, Any]] = None
    while time.monotonic() < deadline:
        op = ctx.call_phase1("GET", f"/api/v1/libraries/{lib_id}/operations/{op_id}")
        last_op = op
        if op.get("state") in ("succeeded", "failed"):
            return op
        time.sleep(0.1)
    raise AssertionError(
        f"Library operation {op_id} did not finish within {timeout}s; last={last_op!r}"
    )
