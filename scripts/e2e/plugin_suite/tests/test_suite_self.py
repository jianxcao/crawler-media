"""Comprehensive self-tests for scripts/e2e/plugin_suite (Python 3.9+ stdlib)."""
from __future__ import annotations

import ast
import base64
import builtins
import contextlib
import errno
import hashlib
import http.client
import http.server
import io
import json
import os
from pathlib import Path
import socket
import socketserver
import struct
import subprocess
import sys
import tempfile
import threading
import time
from typing import Tuple
import unittest
import urllib.parse
import uuid
import zipfile

_REPO_ROOT = str(Path(__file__).resolve().parents[4])
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

from scripts.e2e.plugin_suite.lib.client import ApiClient, ApiError
from scripts.e2e.plugin_suite.lib.fixtures import (
    BencodeError,
    NEGATIVE_CMPLUGIN_KINDS,
    bencode_decode,
    bencode_encode,
    build_cmplugin,
    build_dishonest_torrent,
    build_magnet_uri,
    build_malformed_torrent,
    build_missing_file_length_torrent,
    build_negative_cmplugin,
    build_negative_length_torrent,
    build_overflow_torrent,
    build_oversized_torrent_bytes,
    build_secret_config_schema,
    build_torrent_bytes,
    build_v2_only_torrent,
    ed25519_public_key,
    ed25519_sign,
    ed25519_verify,
    generate_minisign_keypair,
    sign_cmplugin_sha256,
    sign_minisign,
    verify_minisign,
)
from scripts.e2e.plugin_suite.lib.instance import InstanceSupervisor, allocate_free_port
from scripts.e2e.plugin_suite.lib.mocks import (
    MAX_WS_FRAME_BYTES,
    MockObscuraCdpServer,
    MockPtSiteServer,
    MockQbitServer,
    _ThreadedHTTPServer,
)
from scripts.e2e.plugin_suite.lib.stub_server import (
    SelfTestStubServer,
    VALID_REAUTH_PURPOSES,
    _ThreadedStubHTTPServer,
)
from scripts.e2e.plugin_suite.run import parse_args, run_suite
from scripts.e2e.plugin_suite.tiers import (
    SuiteRegistry,
    TestCase,
    TestContext,
    UnimplementedFeatureError,
    build_default_registry,
    validate_registry_thresholds,
)
from scripts.e2e.plugin_suite.tiers._helpers import (
    assert_api_error,
    disable_plugin,
    enable_plugin,
    ensure_plugin_enabled,
    extract_prep_grants,
    obtain_reauth,
    prepare_and_install_plugin,
    put_browser_settings,
    run_plugin_cli,
    set_developer_mode,
    trigger_job,
)
from scripts.e2e.plugin_suite.tiers.tier3_combinations import (
    _rewrite_toml_fields,
    _safe_restore_developer_mode,
)


def _ws_client_send_text(sock: socket.socket, text: str) -> None:
    payload = text.encode("utf-8")
    mask_key = b"\x12\x34\x56\x78"
    header = bytearray([0x81])
    n = len(payload)
    if n < 126:
        header.append(0x80 | n)
    elif n < 65536:
        header.append(0x80 | 126)
        header.extend(struct.pack("!H", n))
    else:
        header.append(0x80 | 127)
        header.extend(struct.pack("!Q", n))
    header.extend(mask_key)
    masked = bytes(b ^ mask_key[i % 4] for i, b in enumerate(payload))
    sock.sendall(bytes(header) + masked)


def _ws_client_recv_json(sock: socket.socket) -> dict:
    def _recv_exact(n: int) -> bytes:
        buf = bytearray()
        while len(buf) < n:
            chunk = sock.recv(n - len(buf))
            if not chunk:
                raise EOFError("Unexpected EOF from WebSocket")
            buf.extend(chunk)
        return bytes(buf)

    hdr = _recv_exact(2)
    length = hdr[1] & 0x7F
    if length == 126:
        length = struct.unpack("!H", _recv_exact(2))[0]
    elif length == 127:
        length = struct.unpack("!Q", _recv_exact(8))[0]
    payload = _recv_exact(length)
    return json.loads(payload.decode("utf-8"))


class TestEd25519AndMinisign(unittest.TestCase):
    def test_rfc8032_test_vectors(self) -> None:
        # RFC 8032 Section 7.1 TEST 1 (empty message)
        sk1 = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
        pk1 = bytes.fromhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
        sig1 = bytes.fromhex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155"
            "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        )
        self.assertEqual(ed25519_public_key(sk1), pk1)
        self.assertEqual(ed25519_sign(sk1, b""), sig1)
        self.assertTrue(ed25519_verify(pk1, b"", sig1))
        self.assertFalse(ed25519_verify(pk1, b"x", sig1))

        # RFC 8032 Section 7.1 TEST 2 (1-byte message 0x72)
        sk2 = bytes.fromhex("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb")
        pk2 = bytes.fromhex("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c")
        sig2 = bytes.fromhex(
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da"
            "085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"
        )
        self.assertEqual(ed25519_public_key(sk2), pk2)
        self.assertEqual(ed25519_sign(sk2, b"\x72"), sig2)
        self.assertTrue(ed25519_verify(pk2, b"\x72", sig2))

    def test_minisign_prehashed_and_legacy_and_tamper_detection(self) -> None:
        kp = generate_minisign_keypair(seed=b"\x33" * 32, key_id=b"\x01\x02\x03\x04\x05\x06\x07\x08")
        kp_other = generate_minisign_keypair(seed=b"\x44" * 32)
        payload = hashlib.sha256(b"sample cmplugin archive").digest()

        for hashed, expected_alg in ((True, "ED"), (False, "Ed")):
            sig = sign_minisign(payload, kp, hashed=hashed)
            res = verify_minisign(payload, sig, kp.public_key_text)
            self.assertTrue(res.valid, res.reason)
            self.assertEqual(res.algorithm, expected_alg)
            self.assertEqual(res.key_id_hex, "0807060504030201")
            self.assertEqual(kp.key_id_hex, "0807060504030201")
            self.assertIn("0807060504030201", kp.public_key_text)

        # Fingerprint must hash the full 42-byte Ed + key_id + pubkey payload
        expected_fp = hashlib.sha256(b"Ed" + kp.key_id + kp.public_key).hexdigest()
        self.assertEqual(kp.fingerprint, expected_fp)

        # Small-order / identity points must be rejected
        identity_pt = (1).to_bytes(32, "little")
        sig_zero = identity_pt + (0).to_bytes(32, "little")
        self.assertFalse(ed25519_verify(identity_pt, b"anything", sig_zero))

        # Tampered data
        sig_ed = sign_minisign(payload, kp, hashed=True)
        self.assertFalse(verify_minisign(b"wrong-digest", sig_ed, kp.public_key_text).valid)

        # Wrong key
        self.assertFalse(verify_minisign(payload, sig_ed, kp_other.public_key_text).valid)

        # Tampered signature bytes
        bad_sig = sign_minisign(payload, kp, hashed=True, tamper_sig=True)
        self.assertFalse(verify_minisign(payload, bad_sig, kp.public_key_text).valid)

        # Tampered global signature
        bad_global = sign_minisign(payload, kp, hashed=True, tamper_global=True)
        self.assertFalse(verify_minisign(payload, bad_global, kp.public_key_text).valid)

        # Tampered trusted comment
        bad_comment = sign_minisign(payload, kp, hashed=True, tamper_comment=True)
        self.assertFalse(verify_minisign(payload, bad_comment, kp.public_key_text).valid)

        # Wrong key_id in signature header
        bad_kid = sign_minisign(payload, kp, hashed=True, wrong_key_id=b"\xff" * 8)
        self.assertFalse(verify_minisign(payload, bad_kid, kp.public_key_text).valid)


class TestBencodeAndTorrents(unittest.TestCase):
    def test_bencode_roundtrip_and_errors(self) -> None:
        obj = {
            b"announce": b"http://127.0.0.1:6969/announce",
            b"count": 42,
            b"neg": -7,
            b"items": [b"a", b"b", 123],
        }
        encoded = bencode_encode(obj)
        self.assertEqual(bencode_decode(encoded), obj)

        with self.assertRaises(BencodeError):
            bencode_decode(b"i-0e")
        with self.assertRaises(BencodeError):
            bencode_decode(b"i01e")
        with self.assertRaises(BencodeError):
            bencode_decode(b"i-01e")
        with self.assertRaises(BencodeError):
            bencode_decode(b"ie")
        with self.assertRaises(BencodeError):
            bencode_decode(b"i-e")
        for non_canon in (b"i1_0e", b"i+1e", b"i 1e", b"i1 e", b"i-1_0e"):
            with self.assertRaises(BencodeError):
                bencode_decode(non_canon)
        with self.assertRaises(BencodeError):
            bencode_decode(b"d1:b1:x1:a1:ye")  # unsorted keys
        with self.assertRaises(BencodeError):
            bencode_decode(b"1a:x")
        with self.assertRaises(BencodeError):
            bencode_decode(b"1_0:xxxxxxxxxx")
        with self.assertRaises(BencodeError):
            bencode_decode(build_malformed_torrent("deep_nesting"))

    def test_torrent_single_multi_dishonest_and_overflow(self) -> None:
        single = build_torrent_bytes(name="movie.mkv", length=5242880)
        self.assertFalse(single.is_multi_file)
        self.assertEqual(single.verified_total_bytes, 5242880)
        self.assertEqual(len(single.info_hash), 40)
        decoded = bencode_decode(single.bytes)
        expected_ih = hashlib.sha1(bencode_encode(decoded[b"info"])).hexdigest()
        self.assertEqual(single.info_hash, expected_ih)

        multi = build_torrent_bytes(
            name="season-pack",
            files=[(["ep1.mkv"], 1000), (["ep2.mkv"], 2500)],
        )
        self.assertTrue(multi.is_multi_file)
        self.assertEqual(multi.verified_total_bytes, 3500)

        dishonest = build_dishonest_torrent(actual_bytes=1024 ** 4)
        self.assertEqual(dishonest.verified_total_bytes, 1024 ** 4)

        overflow = build_overflow_torrent()
        self.assertEqual(overflow.verified_total_bytes, 2 ** 63)

        neg_raw = build_negative_length_torrent()
        self.assertEqual(bencode_decode(neg_raw)[b"info"][b"length"], -1)


class TestCmpluginFixtures(unittest.TestCase):
    def test_valid_cmplugin_variants_deterministic(self) -> None:
        p1 = build_cmplugin(kind="hook")
        p2 = build_cmplugin(kind="hook")
        self.assertEqual(p1.bytes, p2.bytes)
        self.assertEqual(p1.sha256_hex, p2.sha256_hex)
        self.assertIn("components/policy.wasm", p1.entries)
        self.assertIn("plugin.toml", p1.entries)
        self.assertIn('permissions = ["hooks:search.after", "kv"]', p1.manifest_toml)
        self.assertIn('id = "search.after"', p1.manifest_toml)
        self.assertNotIn("subscribe.score", p1.manifest_toml)

        p_custom = build_cmplugin(
            kind="hook",
            hooks=[{"id": "subscribe.score", "priority": 250}],
        )
        self.assertIn('permissions = ["hooks:subscribe.score", "kv"]', p_custom.manifest_toml)
        self.assertIn('id = "subscribe.score"', p_custom.manifest_toml)

        t_pkg = build_cmplugin(kind="task", include_ui=True)
        self.assertIn("components/tasks.wasm", t_pkg.entries)
        self.assertIn("ui/asset-manifest.json", t_pkg.entries)

        e_pkg = build_cmplugin(kind="endpoint")
        self.assertIn("components/endpoint.wasm", e_pkg.entries)

        y_pkg = build_cmplugin(kind="yaml-only")
        self.assertIn("sites/e2e-demo.yaml", y_pkg.entries)
        self.assertNotIn("sites/demo.yaml", y_pkg.entries)
        with zipfile.ZipFile(io.BytesIO(y_pkg.bytes), "r") as yzf:
            yaml_text = yzf.read("sites/e2e-demo.yaml").decode("utf-8")
            self.assertIn("id: e2e-demo", yaml_text)
            self.assertIn("search:", yaml_text)
            self.assertIn("path: /torrents.php", yaml_text)
            self.assertIn("query_param: search", yaml_text)

        y_colliding = build_cmplugin(kind="yaml-only", site_id="demo")
        self.assertIn("sites/demo.yaml", y_colliding.entries)
        with zipfile.ZipFile(io.BytesIO(y_colliding.bytes), "r") as yzf_col:
            self.assertIn("id: demo", yzf_col.read("sites/demo.yaml").decode("utf-8"))

        u_pkg = build_cmplugin(kind="ui-only")
        self.assertIn("ui/assets/remoteEntry.js", u_pkg.entries)

        kp = generate_minisign_keypair(seed=b"\x55" * 32)
        sig = sign_cmplugin_sha256(p1.bytes, kp)
        self.assertTrue(verify_minisign(p1.sha256_bytes, sig, kp.public_key_text).valid)

    def test_all_negative_cmplugin_hazards(self) -> None:
        for kind in NEGATIVE_CMPLUGIN_KINDS:
            raw = build_negative_cmplugin(kind)
            with zipfile.ZipFile(io.BytesIO(raw), "r") as zf:
                infos = zf.infolist()
                names = [i.filename for i in infos]
                if kind == "path_traversal":
                    self.assertIn("../evil.wasm", names)
                elif kind == "absolute_path":
                    self.assertIn("/abs.wasm", names)
                elif kind == "symlink":
                    policy_info = [i for i in infos if i.filename == "components/policy.wasm"][0]
                    self.assertEqual((policy_info.external_attr >> 16) & 0o170000, 0o120000)
                elif kind == "duplicate_paths":
                    self.assertEqual(names.count("plugin.toml"), 2)
                elif kind == "case_collision":
                    self.assertIn("Plugin.toml", names)
                    self.assertIn("plugin.toml", names)
                elif kind == "encrypted_entry":
                    self.assertTrue(infos[0].flag_bits & 0x1)
                elif kind == "zip_bomb":
                    readme = [i for i in infos if i.filename == "README.md"][0]
                    self.assertGreater(readme.file_size, 10 * 1024 * 1024)
                    self.assertGreater(readme.file_size / max(1, readme.compress_size), 50)
                elif kind == "undeclared_executable":
                    self.assertIn("bin/run.sh", names)
                    run_info = [i for i in infos if i.filename == "bin/run.sh"][0]
                    self.assertEqual((run_info.external_attr >> 16) & 0o777, 0o755)
                elif kind == "undeclared_wasm":
                    self.assertIn("components/extra.wasm", names)
                elif kind == "empty_package":
                    toml_text = zf.read("plugin.toml").decode("utf-8")
                    self.assertNotIn("[[components]]", toml_text)
                elif kind == "invalid_ui_too_many_files":
                    self.assertGreater(len(names), 1024)
                elif kind == "invalid_toml_unknown_field":
                    self.assertIn("unexpected_secret_field", zf.read("plugin.toml").decode("utf-8"))
                elif kind == "invalid_toml_bad_semver":
                    self.assertIn('version = ">=1.0"', zf.read("plugin.toml").decode("utf-8"))
                elif kind == "invalid_toml_bad_api":
                    self.assertIn('api = "^1.0"', zf.read("plugin.toml").decode("utf-8"))
                elif kind == "invalid_toml_bad_world":
                    self.assertIn("unknown-world", zf.read("plugin.toml").decode("utf-8"))
                elif kind == "invalid_toml_reserved_id":
                    self.assertIn('id = "installed"', zf.read("plugin.toml").decode("utf-8"))
                elif kind == "invalid_schema_remote_ref":
                    self.assertIn("https://evil.invalid", zf.read("schemas/config.schema.json").decode("utf-8"))
                elif kind == "invalid_schema_recursive_ref":
                    self.assertIn("#/$defs/node", zf.read("schemas/config.schema.json").decode("utf-8"))
                elif kind == "invalid_schema_bad_widget":
                    self.assertIn("raw-html-eval", zf.read("schemas/config.schema.json").decode("utf-8"))
                elif kind == "invalid_ui_html":
                    self.assertIn("ui/assets/index.html", names)
                elif kind == "invalid_ui_map":
                    self.assertIn("ui/assets/remoteEntry.js.map", names)
                elif kind == "invalid_ui_wrong_sha256":
                    manifest = json.loads(zf.read("ui/asset-manifest.json").decode("utf-8"))
                    self.assertEqual(len(manifest["files"]), 2)
                    self.assertEqual(manifest["files"][0]["sha256"], "0" * 64)
                    first_path = manifest["files"][0]["path"]
                    second = manifest["files"][1]
                    self.assertEqual(manifest["files"][0]["size"], len(zf.read(first_path)))
                    self.assertEqual(second["size"], len(zf.read(second["path"])))
                    self.assertEqual(
                        second["sha256"],
                        hashlib.sha256(zf.read(second["path"])).hexdigest(),
                    )
                elif kind == "invalid_ui_oversized_file":
                    huge = [i for i in infos if i.filename == "ui/assets/huge.js"][0]
                    self.assertGreater(huge.file_size, 8 * 1024 * 1024)
                    ratio = huge.file_size / max(1, huge.compress_size)
                    self.assertLess(ratio, 5.0)


class TestMockQbitServer(unittest.TestCase):
    def test_qbit_10_endpoint_lifecycle(self) -> None:
        with MockQbitServer() as qb:
            c = ApiClient(qb.url)
            # Unauthenticated info -> 403
            self.assertEqual(c.raw_request("GET", "/api/v2/torrents/info").status, 403)

            # 1. Login
            login_r = c.raw_request(
                "POST",
                "/api/v2/auth/login",
                body=b"username=admin&password=adminadmin",
                headers={"Content-Type": "application/x-www-form-urlencoded"},
            )
            self.assertEqual(login_r.status, 200)
            self.assertEqual(login_r.text(), "Ok.")
            sid_hdr = login_r.header("set-cookie", "")
            self.assertIn("SID=", sid_hdr)
            cookie_hdr = {"Cookie": sid_hdr.split(";", 1)[0]}

            # 2. Multipart torrent add
            tf = build_torrent_bytes(name="qbit-test-movie", length=8192)
            add_r = c.post_multipart(
                "/api/v2/torrents/add",
                fields={"tags": "crawler-media,crawler-media-owned-1", "category": "movies", "paused": "false"},
                files=[("torrents", "test.torrent", tf.bytes, "application/x-bittorrent")],
                headers=cookie_hdr,
                expect_envelope=False,
            )
            self.assertEqual(add_r.status, 200)
            self.assertEqual(add_r.text(), "Ok.")

            # Foreign torrent seeding
            foreign_hash = "0" * 40
            qb.seed_foreign_torrent(foreign_hash, name="foreign-iso", size=4096, tags="other")

            # 3. Info filter by tag & hashes & category
            info_tag = c.raw_request(
                "GET",
                "/api/v2/torrents/info?tag=crawler-media",
                headers=cookie_hdr,
            ).json()
            self.assertEqual(len(info_tag), 1)
            self.assertEqual(info_tag[0]["hash"], tf.info_hash)

            info_both = c.raw_request(
                "GET",
                f"/api/v2/torrents/info?hashes={tf.info_hash}|{foreign_hash}",
                headers=cookie_hdr,
            ).json()
            self.assertEqual(len(info_both), 2)

            # 4. Files + 5. filePrio
            files = c.raw_request(
                "GET", f"/api/v2/torrents/files?hash={tf.info_hash}", headers=cookie_hdr
            ).json()
            self.assertEqual(files[0]["priority"], 1)
            c.raw_request(
                "POST",
                "/api/v2/torrents/filePrio",
                body=f"hash={tf.info_hash}&id=0&priority=0".encode("utf-8"),
                headers={**cookie_hdr, "Content-Type": "application/x-www-form-urlencoded"},
            )
            files_after = c.raw_request(
                "GET", f"/api/v2/torrents/files?hash={tf.info_hash}", headers=cookie_hdr
            ).json()
            self.assertEqual(files_after[0]["priority"], 0)

            # 6. addTags
            c.raw_request(
                "POST",
                "/api/v2/torrents/addTags",
                body=f"hashes={foreign_hash}&tags=crawler-media-claimed,crawler-media".encode("utf-8"),
                headers={**cookie_hdr, "Content-Type": "application/x-www-form-urlencoded"},
            )
            self.assertIn("crawler-media-claimed", qb.torrents[foreign_hash]["tags"])

            # 7. pause & 8. resume
            c.raw_request(
                "POST",
                "/api/v2/torrents/pause",
                body=f"hashes={tf.info_hash}".encode("utf-8"),
                headers={**cookie_hdr, "Content-Type": "application/x-www-form-urlencoded"},
            )
            self.assertEqual(qb.torrents[tf.info_hash]["state"], "pausedDL")
            c.raw_request(
                "POST",
                "/api/v2/torrents/resume",
                body=f"hashes={tf.info_hash}".encode("utf-8"),
                headers={**cookie_hdr, "Content-Type": "application/x-www-form-urlencoded"},
            )
            self.assertEqual(qb.torrents[tf.info_hash]["state"], "downloading")

            # 9. app/preferences & setPreferences
            c.raw_request(
                "POST",
                "/api/v2/app/setPreferences",
                body=b'json={"dl_limit":102400,"up_limit":51200}',
                headers={**cookie_hdr, "Content-Type": "application/x-www-form-urlencoded"},
            )
            prefs = c.raw_request("GET", "/api/v2/app/preferences", headers=cookie_hdr).json()
            self.assertEqual(prefs["dl_limit"], 102400)
            self.assertEqual(prefs["up_limit"], 51200)

            # 403 relogin trigger must invalidate SID across retries until /api/v2/auth/login
            old_sid_cookie = cookie_hdr["Cookie"]
            qb.trigger_relogin_on_next_call()
            self.assertEqual(
                c.raw_request("GET", "/api/v2/torrents/info", headers=cookie_hdr).status, 403
            )
            self.assertEqual(
                c.raw_request("GET", "/api/v2/torrents/info", headers=cookie_hdr).status, 403
            )
            relogin_r = c.raw_request(
                "POST",
                "/api/v2/auth/login",
                body=b"username=admin&password=adminadmin",
                headers={"Content-Type": "application/x-www-form-urlencoded"},
            )
            self.assertEqual(relogin_r.status, 200)
            new_sid_cookie = relogin_r.header("set-cookie", "").split(";", 1)[0]
            self.assertNotEqual(new_sid_cookie, old_sid_cookie)
            cookie_hdr = {"Cookie": new_sid_cookie}
            self.assertEqual(
                c.raw_request("GET", "/api/v2/torrents/info", headers=cookie_hdr).status, 200
            )

            # Duplicate torrents/add must preserve pre-seeded foreign torrent state/tags
            dup_tf = build_torrent_bytes(name="dup-attempt", length=12345)
            qb.seed_foreign_torrent(
                dup_tf.info_hash,
                name="existing-foreign",
                size=999999,
                tags="foreign-keep",
                state="uploading",
                progress=1.0,
            )
            dup_r = c.post_multipart(
                "/api/v2/torrents/add",
                fields={"tags": "crawler-media", "paused": "false"},
                files=[("torrents", "dup.torrent", dup_tf.bytes, "application/x-bittorrent")],
                headers=cookie_hdr,
                expect_envelope=False,
            )
            self.assertEqual(dup_r.status, 200)
            self.assertEqual(qb.torrents[dup_tf.info_hash]["tags"], "foreign-keep")
            self.assertEqual(qb.torrents[dup_tf.info_hash]["state"], "uploading")
            self.assertEqual(qb.torrents[dup_tf.info_hash]["progress"], 1.0)
            self.assertEqual(qb.torrents[dup_tf.info_hash]["size"], 999999)
            c.raw_request(
                "POST",
                "/api/v2/torrents/delete",
                body=f"hashes={dup_tf.info_hash}&deleteFiles=false".encode("utf-8"),
                headers={**cookie_hdr, "Content-Type": "application/x-www-form-urlencoded"},
            )

            # 10. delete honoring deleteFiles true vs false
            c.raw_request(
                "POST",
                "/api/v2/torrents/delete",
                body=f"hashes={foreign_hash}&deleteFiles=false".encode("utf-8"),
                headers={**cookie_hdr, "Content-Type": "application/x-www-form-urlencoded"},
            )
            c.raw_request(
                "POST",
                "/api/v2/torrents/delete",
                body=f"hashes={tf.info_hash}&deleteFiles=true".encode("utf-8"),
                headers={**cookie_hdr, "Content-Type": "application/x-www-form-urlencoded"},
            )
            self.assertEqual(qb.deleted_log[-2]["deleteFiles"], False)
            self.assertEqual(qb.deleted_log[-1]["deleteFiles"], True)
            self.assertEqual(len(qb.torrents), 0)


class TestMockPtSiteAndCdpServers(unittest.TestCase):
    def test_pt_site_nexusphp_mteam_and_redirects(self) -> None:
        with MockPtSiteServer(
            expected_cookie="pass=valid",
            expected_api_key="secret-mteam-key",
        ) as pt:
            c = ApiClient(pt.url)
            # NexusPHP index.php & torrents.php (exact cookie pair matching)
            self.assertIn("logout.php", c.raw_request("GET", "/index.php", headers={"Cookie": "pass=valid"}).text())
            self.assertIn("logout.php", c.raw_request("GET", "/index.php", headers={"Cookie": "other=1; pass=valid; foo=bar"}).text())
            self.assertIn("login.php", c.raw_request("GET", "/index.php", headers={"Cookie": "pass=bad"}).text())
            self.assertIn("login.php", c.raw_request("GET", "/index.php", headers={"Cookie": "pass=valid_but_wrong"}).text())
            self.assertIn("login.php", c.raw_request("GET", "/index.php", headers={"Cookie": "xpass=valid"}).text())
            self.assertIn("tr class='torrent'", c.raw_request("GET", "/torrents.php?search=Demo", headers={"Cookie": "pass=valid"}).text())
            self.assertIn("<rss version='2.0'>", c.raw_request("GET", "/rss.php").text())

            # Attendance with Set-Cookie rotation and modes
            pt.checkin_rotate_cookie = "nexusphp_u2=rotated99"
            att = c.raw_request("POST", "/attendance.php", headers={"Cookie": "pass=valid"})
            self.assertIn("签到成功", att.text())
            self.assertIn("nexusphp_u2=rotated99", att.header("set-cookie", ""))
            pt.checkin_mode = "already"
            self.assertIn("您今天已经签到过了", c.raw_request("POST", "/attendance.php", headers={"Cookie": "pass=valid"}).text())
            pt.checkin_mode = "failure"
            self.assertIn("签到失败", c.raw_request("POST", "/attendance.php", headers={"Cookie": "pass=valid"}).text())
            pt.checkin_mode = "challenge"
            self.assertIn("cf-chl-", c.raw_request("POST", "/attendance.php", headers={"Cookie": "pass=valid"}).text())

            # /redirect/to and /downgrade
            r_to = c.raw_request("GET", "/redirect/to?url=http://127.0.0.1:9999/internal")
            self.assertEqual(r_to.status, 302)
            self.assertEqual(r_to.header("location"), "http://127.0.0.1:9999/internal")
            r_dg = c.raw_request("GET", "/downgrade")
            self.assertEqual(r_dg.status, 302)
            self.assertTrue(r_dg.header("location", "").endswith("/spy"))

            # M-Team JSON API
            mt_search = c.raw_request(
                "POST",
                "/api/torrent/search",
                body=b'{"keyword":"Demo"}',
                headers={"x-api-key": "secret-mteam-key", "Content-Type": "application/json"},
            ).json()
            self.assertEqual(mt_search["code"], "0")
            self.assertEqual(len(mt_search["data"]["data"]), 1)

            mt_dl = c.raw_request(
                "POST",
                "/api/torrent/genDlToken",
                body=b"id=101",
                headers={"x-api-key": "secret-mteam-key", "Content-Type": "application/x-www-form-urlencoded"},
            ).json()
            self.assertEqual(mt_dl["code"], "0")
            self.assertTrue(mt_dl["data"].endswith("/torrents/101.torrent"))

            # Redirect chain (non-following client) + header spy
            redir = c.raw_request("GET", "/redirect/3")
            self.assertEqual(redir.status, 302)
            self.assertTrue(redir.header("location", "").endswith("/redirect/2"))

            c.raw_request("GET", "/spy", headers={"Authorization": "Bearer leak-check"})
            self.assertEqual(len(pt.spy_requests), 1)
            self.assertEqual(pt.spy_requests[0]["headers"]["authorization"], "Bearer leak-check")

    def test_obscura_cdp_websocket_rpc_and_incompatible_mode(self) -> None:
        with MockObscuraCdpServer() as cdp:
            c = ApiClient(cdp.url)
            ver = c.raw_request("GET", "/json/version").json()
            self.assertEqual(ver["webSocketDebuggerUrl"], f"ws://127.0.0.1:{cdp.port}/ws/browser")

            parts = urllib.parse.urlsplit(ver["webSocketDebuggerUrl"])
            sock = socket.create_connection((parts.hostname, parts.port), timeout=5.0)
            try:
                key = base64.b64encode(b"0123456789abcdef").decode("ascii")
                req = (
                    f"GET {parts.path} HTTP/1.1\r\n"
                    f"Host: {parts.netloc}\r\n"
                    f"Upgrade: websocket\r\n"
                    f"Connection: Upgrade\r\n"
                    f"Sec-WebSocket-Key: {key}\r\n"
                    f"Sec-WebSocket-Version: 13\r\n\r\n"
                ).encode("ascii")
                sock.sendall(req)
                buf = b""
                while b"\r\n\r\n" not in buf:
                    buf += sock.recv(1024)
                self.assertIn(b"101 Switching Protocols", buf)

                _ws_client_send_text(sock, json.dumps({"id": 1, "method": "Target.createBrowserContext", "params": {"disposeOnDetach": True}}))
                r1 = _ws_client_recv_json(sock)
                ctx_id = r1["result"]["browserContextId"]
                self.assertTrue(ctx_id.startswith("ctx-"))

                _ws_client_send_text(sock, json.dumps({"id": 2, "method": "Target.createTarget", "params": {"url": "about:blank", "browserContextId": ctx_id}}))
                tgt_id = _ws_client_recv_json(sock)["result"]["targetId"]

                _ws_client_send_text(sock, json.dumps({"id": 3, "method": "Target.attachToTarget", "params": {"targetId": tgt_id, "flatten": True}}))
                sid = _ws_client_recv_json(sock)["result"]["sessionId"]

                _ws_client_send_text(sock, json.dumps({"id": 4, "sessionId": sid, "method": "Network.setCookies", "params": {"cookies": [{"name": "uid", "value": "99", "domain": "127.0.0.1"}]}}))
                self.assertIn("result", _ws_client_recv_json(sock))

                _ws_client_send_text(sock, json.dumps({"id": 5, "sessionId": sid, "method": "Page.navigate", "params": {"url": "http://127.0.0.1/index.php"}}))
                nav_res = _ws_client_recv_json(sock)
                nav_evt = _ws_client_recv_json(sock)
                self.assertIn("loaderId", nav_res["result"])
                self.assertEqual(nav_evt["method"], "Page.lifecycleEvent")
                self.assertEqual(nav_evt["params"]["name"], "load")

                _ws_client_send_text(sock, json.dumps({"id": 6, "sessionId": sid, "method": "Runtime.evaluate", "params": {"expression": "document.readyState"}}))
                eval_res = _ws_client_recv_json(sock)
                self.assertEqual(eval_res["result"]["result"]["value"], "complete")

                _ws_client_send_text(sock, json.dumps({"id": 7, "sessionId": sid, "method": "Network.getCookies", "params": {"urls": ["http://127.0.0.1"]}}))
                cookies_res = _ws_client_recv_json(sock)
                self.assertEqual(len(cookies_res["result"]["cookies"]), 1)

                _ws_client_send_text(sock, json.dumps({"id": 8, "method": "Target.closeTarget", "params": {"targetId": tgt_id}}))
                self.assertIn("result", _ws_client_recv_json(sock))
                _ws_client_send_text(sock, json.dumps({"id": 9, "method": "Target.disposeBrowserContext", "params": {"browserContextId": ctx_id}}))
                self.assertIn("result", _ws_client_recv_json(sock))

                # Fragmented RFC 6455 text frame with interleaved Ping control frame
                frag_msg = json.dumps(
                    {"id": 10, "method": "Target.createBrowserContext", "params": {"disposeOnDetach": True}}
                ).encode("utf-8")
                split_idx = len(frag_msg) // 2
                part1 = frag_msg[:split_idx]
                part2 = frag_msg[split_idx:]
                mask1 = b"\x01\x02\x03\x04"
                sock.sendall(
                    bytes([0x01, 0x80 | len(part1)])
                    + mask1
                    + bytes(b ^ mask1[i % 4] for i, b in enumerate(part1))
                )
                ping_payload = b"ping-mid"
                mask_ping = b"\x05\x06\x07\x08"
                sock.sendall(
                    bytes([0x89, 0x80 | len(ping_payload)])
                    + mask_ping
                    + bytes(b ^ mask_ping[i % 4] for i, b in enumerate(ping_payload))
                )
                mask2 = b"\x09\x0a\x0b\x0c"
                sock.sendall(
                    bytes([0x80, 0x80 | len(part2)])
                    + mask2
                    + bytes(b ^ mask2[i % 4] for i, b in enumerate(part2))
                )
                pong_hdr = sock.recv(2 + len(ping_payload))
                self.assertEqual(pong_hdr[0] & 0x0F, 0xA)
                self.assertEqual(pong_hdr[2:], ping_payload)
                frag_res = _ws_client_recv_json(sock)
                self.assertEqual(frag_res["id"], 10)
                self.assertTrue(frag_res["result"]["browserContextId"].startswith("ctx-"))
            finally:
                sock.close()

        # Post-navigation timeout mode omits Page.lifecycleEvent
        with MockObscuraCdpServer(post_nav_timeout=True) as timeout_cdp:
            parts = urllib.parse.urlsplit(timeout_cdp.ws_url)
            sock = socket.create_connection((parts.hostname, parts.port), timeout=5.0)
            try:
                key = base64.b64encode(b"1234567890abcdef").decode("ascii")
                sock.sendall(
                    (
                        f"GET {parts.path} HTTP/1.1\r\n"
                        f"Host: {parts.netloc}\r\n"
                        f"Upgrade: websocket\r\n"
                        f"Connection: Upgrade\r\n"
                        f"Sec-WebSocket-Key: {key}\r\n"
                        f"Sec-WebSocket-Version: 13\r\n\r\n"
                    ).encode("ascii")
                )
                buf = b""
                while b"\r\n\r\n" not in buf:
                    buf += sock.recv(1024)
                _ws_client_send_text(sock, json.dumps({"id": 1, "sessionId": "sid-1", "method": "Page.navigate", "params": {"url": "http://127.0.0.1"}}))
                nav_res = _ws_client_recv_json(sock)
                self.assertIn("loaderId", nav_res["result"])
                sock.settimeout(0.2)
                with self.assertRaises(socket.timeout):
                    _ws_client_recv_json(sock)
            finally:
                sock.close()

        # Incompatible mode returns -32601 on Target.createBrowserContext
        with MockObscuraCdpServer(incompatible_mode=True) as bad_cdp:
            parts = urllib.parse.urlsplit(bad_cdp.ws_url)
            sock = socket.create_connection((parts.hostname, parts.port), timeout=5.0)
            try:
                key = base64.b64encode(b"abcdef0123456789").decode("ascii")
                sock.sendall(
                    (
                        f"GET {parts.path} HTTP/1.1\r\n"
                        f"Host: {parts.netloc}\r\n"
                        f"Upgrade: websocket\r\n"
                        f"Connection: Upgrade\r\n"
                        f"Sec-WebSocket-Key: {key}\r\n"
                        f"Sec-WebSocket-Version: 13\r\n\r\n"
                    ).encode("ascii")
                )
                buf = b""
                while b"\r\n\r\n" not in buf:
                    buf += sock.recv(1024)
                _ws_client_send_text(sock, json.dumps({"id": 1, "method": "Target.createBrowserContext", "params": {}}))
                err = _ws_client_recv_json(sock)
                self.assertEqual(err["error"]["code"], -32601)
            finally:
                sock.close()

        # Abrupt TCP shutdown mid-session must not break server or subsequent clients
        with MockObscuraCdpServer() as abrupt_cdp:
            parts = urllib.parse.urlsplit(abrupt_cdp.ws_url)
            s1 = socket.create_connection((parts.hostname, parts.port), timeout=5.0)
            key = base64.b64encode(b"fedcba9876543210").decode("ascii")
            s1.sendall(
                (
                    f"GET {parts.path} HTTP/1.1\r\n"
                    f"Host: {parts.netloc}\r\n"
                    f"Upgrade: websocket\r\n"
                    f"Connection: Upgrade\r\n"
                    f"Sec-WebSocket-Key: {key}\r\n"
                    f"Sec-WebSocket-Version: 13\r\n\r\n"
                ).encode("ascii")
            )
            buf = b""
            while b"\r\n\r\n" not in buf:
                buf += s1.recv(1024)
            # Send partial unclosed fragmented frame then drop TCP abruptly
            s1.sendall(bytes([0x01, 0x82, 0x01, 0x02, 0x03, 0x04, 0x7A, 0x7B]))
            try:
                s1.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            s1.close()
            # Server remains responsive
            self.assertEqual(ApiClient(abrupt_cdp.url).raw_request("GET", "/json/version").status, 200)

    def test_obscura_cdp_ws_close_tcp_eof_and_upgrade_reset(self) -> None:
        def _ws_upgrade(ws_url: str) -> socket.socket:
            parts = urllib.parse.urlsplit(ws_url)
            sock = socket.create_connection((parts.hostname, parts.port), timeout=2.0)
            key = base64.b64encode(b"0123456789abcdef").decode("ascii")
            sock.sendall(
                (
                    f"GET {parts.path} HTTP/1.1\r\n"
                    f"Host: {parts.netloc}\r\n"
                    f"Upgrade: websocket\r\n"
                    f"Connection: Upgrade\r\n"
                    f"Sec-WebSocket-Key: {key}\r\n"
                    f"Sec-WebSocket-Version: 13\r\n\r\n"
                ).encode("ascii")
            )
            buf = b""
            while b"\r\n\r\n" not in buf:
                chunk = sock.recv(1024)
                if not chunk:
                    raise EOFError("EOF during WS upgrade")
                buf += chunk
            self.assertIn(b"101 Switching Protocols", buf)
            return sock

        with MockObscuraCdpServer() as cdp:
            # 1. Clean WS Close (0x8) must echo 0x8 and close TCP socket (EOF b"")
            s_close = _ws_upgrade(cdp.ws_url)
            try:
                mask = b"\x11\x22\x33\x44"
                s_close.sendall(bytes([0x88, 0x80]) + mask)
                close_frame = s_close.recv(16)
                self.assertGreaterEqual(len(close_frame), 2)
                self.assertEqual(close_frame[0] & 0x0F, 0x8)
                eof_bytes = s_close.recv(1024)
                self.assertEqual(eof_bytes, b"")
            finally:
                s_close.close()

            # 2. Protocol error (unexpected 0x0 continuation) sends 1002 Close and closes TCP (EOF b"")
            s_err = _ws_upgrade(cdp.ws_url)
            try:
                mask = b"\x55\x66\x77\x88"
                s_err.sendall(bytes([0x80, 0x80]) + mask)
                err_frame = s_err.recv(16)
                self.assertEqual(err_frame[:2], b"\x88\x02")
                self.assertEqual(struct.unpack("!H", err_frame[2:4])[0], 1002)
                eof_bytes = s_err.recv(1024)
                self.assertEqual(eof_bytes, b"")
            finally:
                s_err.close()

            # 3. Abrupt SO_LINGER reset during 101 upgrade write/flush produces zero stderr tracebacks
            parts = urllib.parse.urlsplit(cdp.ws_url)
            stderr_buf = io.StringIO()
            with contextlib.redirect_stderr(stderr_buf):
                for _ in range(5):
                    s_rst = socket.create_connection((parts.hostname, parts.port), timeout=2.0)
                    s_rst.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
                    key = base64.b64encode(b"0123456789abcdef").decode("ascii")
                    s_rst.sendall(
                        (
                            f"GET {parts.path} HTTP/1.1\r\n"
                            f"Host: {parts.netloc}\r\n"
                            f"Upgrade: websocket\r\n"
                            f"Connection: Upgrade\r\n"
                            f"Sec-WebSocket-Key: {key}\r\n"
                            f"Sec-WebSocket-Version: 13\r\n\r\n"
                        ).encode("ascii")
                    )
                    s_rst.close()
                # Ensure handler threads finish and server remains healthy
                self.assertEqual(ApiClient(cdp.url).raw_request("GET", "/json/version").status, 200)
                time.sleep(0.05)
            captured_err = stderr_buf.getvalue()
            self.assertNotIn("Traceback", captured_err)
            self.assertNotIn("BrokenPipeError", captured_err)
            self.assertEqual(captured_err, "")

    def test_obscura_cdp_oversized_and_invalid_ws_frame_lengths(self) -> None:
        def _ws_upgrade(ws_url: str) -> socket.socket:
            parts = urllib.parse.urlsplit(ws_url)
            sock = socket.create_connection((parts.hostname, parts.port), timeout=2.0)
            key = base64.b64encode(b"fedcba0123456789").decode("ascii")
            sock.sendall(
                (
                    f"GET {parts.path} HTTP/1.1\r\n"
                    f"Host: {parts.netloc}\r\n"
                    f"Upgrade: websocket\r\n"
                    f"Connection: Upgrade\r\n"
                    f"Sec-WebSocket-Key: {key}\r\n"
                    f"Sec-WebSocket-Version: 13\r\n\r\n"
                ).encode("ascii")
            )
            buf = b""
            while b"\r\n\r\n" not in buf:
                chunk = sock.recv(1024)
                if not chunk:
                    raise EOFError("EOF during WS upgrade")
                buf += chunk
            self.assertIn(b"101 Switching Protocols", buf)
            return sock

        self.assertEqual(MAX_WS_FRAME_BYTES, 16 * 1024 * 1024)
        cases = [
            (0x7FFFFFFFFFFFFFFF, 1009),
            (0x8000000000000000, 1002),
            (100 * 1024 * 1024, 1009),
        ]
        stderr_buf = io.StringIO()
        with contextlib.redirect_stderr(stderr_buf):
            with MockObscuraCdpServer() as cdp:
                for length_val, expected_close_code in cases:
                    started = time.monotonic()
                    sock = _ws_upgrade(cdp.ws_url)
                    try:
                        # Masked 64-bit length frame header (no payload sent)
                        hdr = bytes([0x81, 0x80 | 127]) + struct.pack("!Q", length_val) + b"\x11\x22\x33\x44"
                        sock.sendall(hdr)
                        close_frame = sock.recv(16)
                        self.assertEqual(close_frame[:2], b"\x88\x02")
                        self.assertEqual(
                            struct.unpack("!H", close_frame[2:4])[0],
                            expected_close_code,
                        )
                        eof_bytes = sock.recv(1024)
                        self.assertEqual(eof_bytes, b"")
                    finally:
                        sock.close()
                    self.assertLess(time.monotonic() - started, 2.0)

                # Verify server remains healthy for normal HTTP and WS RPCs
                self.assertEqual(ApiClient(cdp.url).raw_request("GET", "/json/version").status, 200)
                ok_sock = _ws_upgrade(cdp.ws_url)
                try:
                    msg = json.dumps({"id": 1, "method": "Target.createBrowserContext"}).encode("utf-8")
                    mask = b"\x01\x02\x03\x04"
                    masked_msg = bytes(b ^ mask[i % 4] for i, b in enumerate(msg))
                    ok_sock.sendall(bytes([0x81, 0x80 | len(msg)]) + mask + masked_msg)
                    resp_hdr = ok_sock.recv(2)
                    resp_len = resp_hdr[1] & 0x7F
                    resp_body = json.loads(ok_sock.recv(resp_len).decode("utf-8"))
                    self.assertEqual(resp_body["id"], 1)
                    self.assertIn("browserContextId", resp_body["result"])
                finally:
                    ok_sock.close()
                time.sleep(0.05)

        captured_err = stderr_buf.getvalue()
        self.assertNotIn("OverflowError", captured_err)
        self.assertNotIn("Traceback", captured_err)
        self.assertEqual(captured_err, "")

    def test_obscura_cdp_non_dict_json_and_malformed_text_frames(self) -> None:
        def _send_masked_text(sock: socket.socket, payload: bytes) -> None:
            mask = b"\x05\x06\x07\x08"
            masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            if len(payload) < 126:
                hdr = bytes([0x81, 0x80 | len(payload)])
            else:
                hdr = bytes([0x81, 0x80 | 126]) + struct.pack("!H", len(payload))
            sock.sendall(hdr + mask + masked)

        def _recv_ws_frame(sock: socket.socket) -> Tuple[int, bytes]:
            hdr = sock.recv(2)
            self.assertEqual(len(hdr), 2)
            opcode = hdr[0] & 0x0F
            length = hdr[1] & 0x7F
            if length == 126:
                length = struct.unpack("!H", sock.recv(2))[0]
            data = b""
            while len(data) < length:
                chunk = sock.recv(length - len(data))
                if not chunk:
                    break
                data += chunk
            return opcode, data

        stderr_buf = io.StringIO()
        with contextlib.redirect_stderr(stderr_buf):
            with MockObscuraCdpServer() as cdp:
                parts = urllib.parse.urlsplit(cdp.ws_url)
                sock = socket.create_connection((parts.hostname, parts.port), timeout=2.0)
                try:
                    key = base64.b64encode(b"0011223344556677").decode("ascii")
                    sock.sendall(
                        (
                            f"GET {parts.path} HTTP/1.1\r\n"
                            f"Host: {parts.netloc}\r\n"
                            f"Upgrade: websocket\r\n"
                            f"Connection: Upgrade\r\n"
                            f"Sec-WebSocket-Key: {key}\r\n"
                            f"Sec-WebSocket-Version: 13\r\n\r\n"
                        ).encode("ascii")
                    )
                    buf = b""
                    while b"\r\n\r\n" not in buf:
                        buf += sock.recv(1024)

                    for raw_json in (b"[]", b"123", b"null", b'"hello"'):
                        _send_masked_text(sock, raw_json)
                        op, body = _recv_ws_frame(sock)
                        self.assertEqual(op, 0x1)
                        parsed = json.loads(body.decode("utf-8"))
                        self.assertEqual(parsed["error"]["code"], -32600)
                        self.assertEqual(parsed["error"]["message"], "Invalid Request")

                    # Non-dict params inside dict request must not raise AttributeError
                    _send_masked_text(
                        sock,
                        json.dumps({"id": 9, "method": "Runtime.evaluate", "params": []}).encode("utf-8"),
                    )
                    op, body = _recv_ws_frame(sock)
                    self.assertEqual(op, 0x1)
                    self.assertEqual(json.loads(body.decode("utf-8"))["id"], 9)

                    # Invalid JSON closes cleanly with WS 1007 and TCP EOF
                    _send_masked_text(sock, b"{not-valid-json")
                    op, body = _recv_ws_frame(sock)
                    self.assertEqual(op, 0x8)
                    self.assertEqual(struct.unpack("!H", body[:2])[0], 1007)
                    self.assertEqual(sock.recv(1024), b"")
                finally:
                    sock.close()
                time.sleep(0.05)

        captured_err = stderr_buf.getvalue()
        self.assertNotIn("AttributeError", captured_err)
        self.assertNotIn("Traceback", captured_err)
        self.assertEqual(captured_err, "")

    def test_http_mocks_and_stub_so_linger_abrupt_reset_no_stderr(self) -> None:
        def _abrupt_http_resets(base_url: str, path: str, count: int = 8) -> None:
            parts = urllib.parse.urlsplit(base_url)
            for _ in range(count):
                s = socket.create_connection((parts.hostname, parts.port), timeout=2.0)
                s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
                s.sendall(
                    (
                        f"GET {path} HTTP/1.1\r\n"
                        f"Host: {parts.netloc}\r\n"
                        f"Connection: close\r\n\r\n"
                    ).encode("ascii")
                )
                s.close()

        stderr_buf = io.StringIO()
        with contextlib.redirect_stderr(stderr_buf):
            with (
                MockObscuraCdpServer() as cdp,
                MockQbitServer(require_auth=False) as qb,
                MockPtSiteServer() as pt,
                SelfTestStubServer() as stub,
            ):
                _abrupt_http_resets(cdp.url, "/json/version")
                _abrupt_http_resets(qb.url, "/api/v2/app/preferences")
                _abrupt_http_resets(pt.url, "/torrents.php")
                _abrupt_http_resets(stub.url, "/api/v1/health")
                time.sleep(0.08)

                self.assertEqual(ApiClient(cdp.url).raw_request("GET", "/json/version").status, 200)
                self.assertEqual(ApiClient(qb.url).raw_request("GET", "/api/v2/app/preferences").status, 200)
                self.assertEqual(ApiClient(pt.url).raw_request("GET", "/torrents.php").status, 200)
                self.assertEqual(ApiClient(stub.url).health().get("status"), "ok")

        captured_err = stderr_buf.getvalue()
        self.assertNotIn("BrokenPipeError", captured_err)
        self.assertNotIn("ConnectionResetError", captured_err)
        self.assertNotIn("Traceback", captured_err)
        self.assertEqual(captured_err, "")

        # Verify handle_error preserves default handling for unexpected exceptions
        for srv_cls in (_ThreadedHTTPServer, _ThreadedStubHTTPServer):
            err_buf = io.StringIO()
            dummy_srv = srv_cls(("127.0.0.1", 0), http.server.BaseHTTPRequestHandler)
            try:
                with contextlib.redirect_stderr(err_buf):
                    try:
                        raise RuntimeError("unexpected-server-bug")
                    except RuntimeError:
                        dummy_srv.handle_error(None, ("127.0.0.1", 12345))
                self.assertIn("unexpected-server-bug", err_buf.getvalue())
            finally:
                dummy_srv.server_close()


class TestInstanceSupervisorAndRunner(unittest.TestCase):
    def test_instance_supervisor_validation_and_cleanup(self) -> None:
        with self.assertRaises(ValueError):
            InstanceSupervisor(cli_token="same", admin_password="same")

        port = allocate_free_port()
        self.assertGreater(port, 0)

        expected_parent = Path(os.environ.get("CRAWLER_E2E_TMP_ROOT") or "/tmp").expanduser().resolve()
        sup = InstanceSupervisor()
        root = sup.root_dir
        self.assertTrue(root.is_dir())
        self.assertEqual(root.parent, expected_parent)
        self.assertTrue(root.name.startswith("crawler-e2e-"))
        self.assertTrue((root / "data").is_dir())
        self.assertTrue((root / "logs").is_dir())
        sup.cleanup()
        self.assertFalse(root.exists())

        # Verify CRAWLER_E2E_TMP_ROOT override for both InstanceSupervisor and TestContext
        with tempfile.TemporaryDirectory() as custom_root:
            prev = os.environ.get("CRAWLER_E2E_TMP_ROOT")
            os.environ["CRAWLER_E2E_TMP_ROOT"] = custom_root
            resolved_custom_root = Path(custom_root).resolve()
            try:
                sup_custom = InstanceSupervisor()
                try:
                    self.assertEqual(sup_custom.root_dir.parent, resolved_custom_root)
                    self.assertTrue(sup_custom.root_dir.name.startswith("crawler-e2e-"))
                finally:
                    sup_custom.cleanup()
                self.assertEqual(list(resolved_custom_root.glob("crawler-e2e-*")), [])

                ctx_custom = TestContext(
                    base_url="http://127.0.0.1:1",
                    client=ApiClient("http://127.0.0.1:1"),
                    admin_password="p",
                    cli_token="t",
                )
                try:
                    self.assertEqual(ctx_custom.tmp_dir.parent, resolved_custom_root)
                    self.assertTrue(ctx_custom.tmp_dir.name.startswith("crawler-e2e-case-"))
                finally:
                    ctx_custom.close()
                self.assertEqual(list(resolved_custom_root.glob("crawler-e2e-*")), [])
            finally:
                if prev is None:
                    os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                else:
                    os.environ["CRAWLER_E2E_TMP_ROOT"] = prev

    def test_runner_self_test_end_to_end_and_tmp_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as isolated_tmp_root:
            report_path = Path(isolated_tmp_root) / "self_test_report.json"
            sub_env = os.environ.copy()
            sub_env["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root

            proc = subprocess.run(
                [
                    sys.executable,
                    "scripts/e2e/plugin_suite/run.py",
                    "--self-test",
                    "--json-report",
                    str(report_path),
                ],
                cwd=_REPO_ROOT,
                env=sub_env,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(
                proc.returncode,
                0,
                f"run.py --self-test failed:\nSTDOUT:\n{proc.stdout}\nSTDERR:\n{proc.stderr}",
            )
            report = json.loads(report_path.read_text(encoding="utf-8"))
            self.assertEqual(report["mode"], "self-test")
            self.assertGreaterEqual(report["summary"]["total"], 4)
            self.assertEqual(report["summary"]["failed"], 0)
            tiers_seen = {r["tier"] for r in report["results"]}
            self.assertEqual(tiers_seen, {1, 2, 3, 4})

            leaked = list(Path(isolated_tmp_root).glob("crawler-e2e-*"))
            self.assertEqual(
                leaked,
                [],
                f"Leaked temp dirs after --self-test: {leaked}",
            )

            # Verify --tier and --feature filtering
            filtered = subprocess.run(
                [
                    sys.executable,
                    "scripts/e2e/plugin_suite/run.py",
                    "--self-test",
                    "--tier",
                    "1",
                    "--feature",
                    "8",
                ],
                cwd=_REPO_ROOT,
                env=sub_env,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(filtered.returncode, 0, filtered.stderr)
            self.assertIn("T1-F08-SMOKE-01", filtered.stdout)
            self.assertNotIn("T1-F01-SMOKE-02", filtered.stdout)
            leaked_filtered = list(Path(isolated_tmp_root).glob("crawler-e2e-*"))
            self.assertEqual(
                leaked_filtered,
                [],
                f"Leaked temp dirs after filtered --self-test: {leaked_filtered}",
            )

    def test_relative_paths_lease_retry_env_and_wait_ready_read_only(self) -> None:
        sup = InstanceSupervisor(
            bin_path=Path("target/debug/crawler-media"),
            web_dir=Path("web/dist"),
        )
        try:
            self.assertTrue(sup.bin_path.is_absolute())
            self.assertTrue(sup.web_dir is not None and sup.web_dir.is_absolute())
            env = sup._build_serve_env(port=19999)
            self.assertEqual(env["CRAWLER_MEDIA_LEASE_TTL_MS"], "2000")
            self.assertEqual(env["CRAWLER_MEDIA_LEASE_RETRY_MS"], "250")
        finally:
            sup.cleanup()

        # wait_ready must wait for read_only=False unless allow_read_only=True
        poll_count = {"n": 0}

        class HealthHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: object) -> None:
                return

            def do_GET(self) -> None:
                poll_count["n"] += 1
                is_ro = poll_count["n"] < 3
                body = json.dumps(
                    {"ok": True, "data": {"status": "ok", "read_only": is_ro}}
                ).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        srv = http.server.HTTPServer(("127.0.0.1", 0), HealthHandler)
        srv_thread = threading.Thread(target=srv.serve_forever, daemon=True)
        srv_thread.start()
        sup_wait = InstanceSupervisor()
        try:
            sup_wait.port = int(srv.server_address[1])
            ro_data = sup_wait.wait_ready(timeout=2.0, allow_read_only=True)
            self.assertTrue(ro_data["read_only"])
            rw_data = sup_wait.wait_ready(timeout=2.0)
            self.assertFalse(rw_data["read_only"])
            self.assertGreaterEqual(poll_count["n"], 3)
        finally:
            sup_wait.cleanup()
            srv.shutdown()
            srv.server_close()
            srv_thread.join(timeout=2.0)

    def test_run_suite_client_isolation_across_logout(self) -> None:
        reg = SuiteRegistry()

        @reg.case(id="ISO-01", tier=1, feature_id=8, name="Mutate and logout client")
        def case_logout(ctx: TestContext) -> None:
            ctx.client.logout()
            ctx.client.token = "poisoned"

        @reg.case(id="ISO-02", tier=1, feature_id=8, name="Subsequent case remains authenticated")
        def case_still_authed(ctx: TestContext) -> None:
            me = ctx.client.me()
            self.assertEqual(me["role"], "admin")

        args = parse_args(["--self-test", "--tier", "1", "--feature", "8"])
        report = run_suite(args, registry=reg)
        self.assertEqual(report["summary"]["total"], 2)
        self.assertEqual(report["summary"]["passed"], 2)
        self.assertEqual(report["summary"]["failed"], 0)


class TestApiClientAndConcurrencyRegressions(unittest.TestCase):
    def test_sse_chunked_split_open_stream_and_ci_headers(self) -> None:
        recorded_raw_headers = []

        class SseAndHeaderSrv(socketserver.ThreadingMixIn, http.server.HTTPServer):
            daemon_threads = True
            request_queue_size = 128

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: object) -> None:
                return

            def do_GET(self) -> None:
                if self.path == "/sse-split-chunked":
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.end_headers()
                    for chunk in (b"event: job\ndata: hel", b"lo\n\n"):
                        self.wfile.write(f"{len(chunk):x}\r\n".encode("ascii") + chunk + b"\r\n")
                        self.wfile.flush()
                    self.wfile.write(b"0\r\n\r\n")
                    self.wfile.flush()
                    return
                if self.path == "/sse-open-one":
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.end_headers()
                    chunk = b"event: job\ndata: partial-stream\n\n"
                    self.wfile.write(f"{len(chunk):x}\r\n".encode("ascii") + chunk + b"\r\n")
                    self.wfile.flush()
                    time.sleep(0.8)
                    return
                if self.path == "/sse-open-empty":
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.end_headers()
                    time.sleep(0.8)
                    return
                self.send_response(404)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length", "0") or "0")
                if length > 0:
                    self.rfile.read(length)
                recorded_raw_headers.append(list(self.headers.items()))
                body = json.dumps({"ok": True, "data": {"accepted": True}}).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        srv = SseAndHeaderSrv(("127.0.0.1", 0), Handler)
        thr = threading.Thread(target=srv.serve_forever, daemon=True)
        thr.start()
        try:
            cli = ApiClient(
                f"http://127.0.0.1:{srv.server_address[1]}",
                token="tok-1",
                session_cookie="sess-1",
            )
            split_evts = cli.read_sse("/sse-split-chunked", max_events=5, timeout=1.0)
            self.assertEqual(len(split_evts), 1)
            self.assertEqual(split_evts[0]["event"], "job")
            self.assertEqual(split_evts[0]["data"], "hello")

            open_one = cli.read_sse("/sse-open-one", max_events=5, timeout=0.25)
            self.assertEqual(len(open_one), 1)
            self.assertEqual(open_one[0]["data"], "partial-stream")

            open_empty = cli.read_sse("/sse-open-empty", max_events=5, timeout=0.2)
            self.assertEqual(open_empty, [])

            cli.post(
                "/patch-test",
                json_body={"a": 1},
                headers={
                    "content-type": "application/merge-patch+json",
                    "cookie": "mc_session=override-cookie",
                    "authorization": "Bearer override-tok",
                },
            )
            sent = recorded_raw_headers[-1]
            ct_vals = [v for k, v in sent if k.lower() == "content-type"]
            ck_vals = [v for k, v in sent if k.lower() == "cookie"]
            auth_vals = [v for k, v in sent if k.lower() == "authorization"]
            self.assertEqual(ct_vals, ["application/merge-patch+json"])
            self.assertEqual(ck_vals, ["mc_session=override-cookie"])
            self.assertEqual(auth_vals, ["Bearer override-tok"])
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_12_thread_concurrent_mock_and_stub_servers(self) -> None:
        def _run_concurrent(fn) -> None:
            errors = []
            lock = threading.Lock()

            def worker(tid: int) -> None:
                try:
                    fn(tid)
                except Exception as exc:
                    with lock:
                        errors.append(f"thread {tid}: {type(exc).__name__}: {exc}")

            threads = [threading.Thread(target=worker, args=(i,)) for i in range(12)]
            for t in threads:
                t.start()
            for t in threads:
                t.join(timeout=15.0)
            self.assertEqual(errors, [])

        with MockQbitServer(require_auth=False) as qb:
            def qb_work(tid: int) -> None:
                with ApiClient(qb.url, timeout=5.0) as c:
                    for j in range(15):
                        r = c.raw_request("GET", "/api/v2/app/preferences")
                        if r.status != 200:
                            raise AssertionError(f"Unexpected qbit status {r.status} at {tid}/{j}")

            _run_concurrent(qb_work)

        with MockPtSiteServer() as pt:
            def pt_work(tid: int) -> None:
                with ApiClient(pt.url, timeout=5.0) as c:
                    for j in range(15):
                        r = c.raw_request("GET", "/index.php")
                        if r.status != 200:
                            raise AssertionError(f"Unexpected pt status {r.status} at {tid}/{j}")

            _run_concurrent(pt_work)

        with SelfTestStubServer() as stub:
            def stub_work(tid: int) -> None:
                with ApiClient(stub.url, timeout=5.0) as c:
                    for j in range(15):
                        h = c.health()
                        if h.get("status") != "ok":
                            raise AssertionError(f"Unexpected stub health {h!r} at {tid}/{j}")

            _run_concurrent(stub_work)


class TestE2EM2RegistryAndExtensions(unittest.TestCase):
    def test_default_registry_tier1_and_tier2_thresholds_and_uniqueness(self) -> None:
        reg = build_default_registry()
        cases = reg.cases
        ids = [c.id for c in cases]
        self.assertEqual(len(ids), len(set(ids)), "Duplicate test case IDs in default registry")

        t1_per_feat = {f: 0 for f in range(1, 27)}
        t2_per_feat = {f: 0 for f in range(1, 27)}
        for c in cases:
            self.assertIn(c.tier, (1, 2, 3, 4))
            self.assertGreaterEqual(c.feature_id, 1)
            self.assertLessEqual(c.feature_id, 26)
            if c.tier == 1:
                t1_per_feat[c.feature_id] += 1
                if not c.id.endswith("SMOKE-01") and not c.id.endswith("SMOKE-02"):
                    self.assertFalse(c.self_test, f"{c.id} must have self_test=False")
            elif c.tier == 2:
                t2_per_feat[c.feature_id] += 1
                if not c.id.endswith("SMOKE-01") and not c.id.endswith("SMOKE-02"):
                    self.assertFalse(c.self_test, f"{c.id} must have self_test=False")

        for fid in range(1, 27):
            self.assertGreaterEqual(
                t1_per_feat[fid], 5, f"Feature {fid} has < 5 Tier 1 cases: {t1_per_feat[fid]}"
            )
            self.assertGreaterEqual(
                t2_per_feat[fid], 5, f"Feature {fid} has < 5 Tier 2 cases: {t2_per_feat[fid]}"
            )
        self.assertGreaterEqual(sum(t1_per_feat.values()), 130)
        self.assertGreaterEqual(sum(t2_per_feat.values()), 130)

    def test_404_target_triggers_unimplemented_never_fake_pass(self) -> None:
        with SelfTestStubServer() as stub:
            args = parse_args(["--target-url", stub.url, "--tier", "1", "--feature", "1"])
            reg = SuiteRegistry()
            for c in build_default_registry().filter(tiers=[1], feature_id=1, self_test=False):
                if c.id == "T1-F01-02":
                    reg.register(c)
                    break
            report = run_suite(args, registry=reg)
            self.assertEqual(report["summary"]["total"], 1)
            self.assertEqual(report["summary"]["passed"], 0)
            self.assertEqual(report["summary"]["unimplemented"], 1)

    def test_instance_supervisor_enter_cleans_up_on_start_failure(self) -> None:
        missing_bin = Path("/tmp/definitely-missing-crawler-media-binary-xyz")
        sup = InstanceSupervisor(bin_path=missing_bin)
        root = sup.root_dir
        self.assertTrue(root.is_dir())
        with self.assertRaises(FileNotFoundError):
            with sup:
                pass
        self.assertFalse(root.exists(), "InstanceSupervisor.__enter__ leaked temp root on start failure")

    def test_new_fixtures_and_negative_cmplugins(self) -> None:
        sec_schema = build_secret_config_schema()
        self.assertTrue(sec_schema["properties"]["api_token"]["writeOnly"])
        self.assertIn("profile_label", sec_schema["properties"])
        self.assertIn("profile_label", sec_schema["required"])
        self.assertNotIn("site_id", sec_schema["properties"])

        idx_pkg = build_cmplugin(plugin_id="org.example.idx-unit", kind="indexer")
        self.assertIn("indexer-plugin@1.0.0", idx_pkg.manifest_toml)
        self.assertIn("[[components.indexers]]", idx_pkg.manifest_toml)

        maint_pkg = build_cmplugin(plugin_id="org.example.maint-unit", kind="maintenance")
        self.assertIn("maintenance-plugin@1.0.0", maint_pkg.manifest_toml)
        self.assertIn("[[components.maintenance]]", maint_pkg.manifest_toml)

        ui_pkg = build_cmplugin(
            plugin_id="org.example.ui-unit",
            kind="ui-only",
            ui_overrides={"remote_name": "cm_unit_remote"},
            extra_ui_assets={"ui/assets/extra.css": (b".x{}\n", "text/css")},
        )
        self.assertIn("ui/assets/extra.css", ui_pkg.entries)
        self.assertIn('remote_name = "cm_unit_remote"', ui_pkg.manifest_toml)

        # New torrent & magnet builders
        ml_decoded = bencode_decode(build_missing_file_length_torrent())
        self.assertNotIn(b"length", ml_decoded[b"info"][b"files"][0])
        v2_decoded = bencode_decode(build_v2_only_torrent())
        self.assertEqual(v2_decoded[b"info"][b"meta version"], 2)
        ov_bytes = build_oversized_torrent_bytes(size_bytes=10 * 1024 * 1024 + 512)
        self.assertGreater(len(ov_bytes), 10 * 1024 * 1024)
        self.assertIn("xt=urn:btih:", build_magnet_uri("a" * 40))

        # New negative kinds spot-checks
        for k in (
            "winzip_aes_encrypted",
            "multi_disk_or_zip64",
            "crc_mismatch",
            "invalid_toml_html_metadata",
            "invalid_toml_cross_world_table",
            "invalid_toml_unsupported_hook",
            "invalid_toml_phase2_world",
            "invalid_schema_cron_format",
            "invalid_schema_secret_in_array",
            "invalid_schema_additional_props_true",
            "invalid_schema_depth_exceeded",
            "invalid_ui_self_listed_manifest",
            "invalid_ui_mime_mismatch",
            "invalid_ui_uppercase_sha256",
            "bundled_pubkey_zip",
            "invalid_yaml_builtin_collision",
            "invalid_http_wildcard_origin",
            "invalid_hook_priority_oob",
            "invalid_ui_contract_mismatch",
            "invalid_ui_react_mismatch",
            "invalid_ui_reserved_remote_name",
            "invalid_ui_too_many_surfaces",
            "invalid_ui_duplicate_config_surface",
            "invalid_ui_invalid_icon",
            "invalid_ui_manifest_version",
            "invalid_ui_missing_entry_in_manifest",
            "invalid_ui_undeclared_asset",
        ):
            self.assertIn(k, NEGATIVE_CMPLUGIN_KINDS)
            raw = build_negative_cmplugin(k)
            self.assertGreater(len(raw), 22)

    def test_new_mock_server_extensions(self) -> None:
        with tempfile.TemporaryDirectory() as save_tmp:
            save_dir = Path(save_tmp) / "qb_files"
            with MockQbitServer(require_auth=False, disk_free_bytes=123456, real_save_dir=save_dir) as qb:
                with ApiClient(qb.url) as c:
                    maindata = c.raw_request("GET", "/api/v2/sync/maindata").json()
                    self.assertEqual(maindata["server_state"]["free_space_on_disk"], 123456)
                    qb.disk_endpoint_error = True
                    self.assertEqual(c.raw_request("GET", "/api/v2/sync/maindata").status, 500)

                    tf = build_torrent_bytes(name="mock_ext_item", length=4096)
                    qb.drop_response_on_next_add = True
                    with self.assertRaises((http.client.HTTPException, ConnectionResetError, ConnectionAbortedError, BrokenPipeError, EOFError)):
                        c.post_multipart(
                            "/api/v2/torrents/add",
                            fields={"tags": "t1,t2"},
                            files=[("torrents", "x.torrent", tf.bytes, "application/x-bittorrent")],
                            expect_envelope=False,
                        )
                    self.assertIn(tf.info_hash, qb.torrents)
                    self.assertTrue((save_dir / "mock_ext_item.mkv").is_file())

                    c.raw_request(
                        "POST",
                        "/api/v2/torrents/setCategory",
                        body=f"hashes={tf.info_hash}&category=boost".encode("utf-8"),
                        headers={"Content-Type": "application/x-www-form-urlencoded"},
                    )
                    self.assertEqual(qb.torrents[tf.info_hash]["category"], "boost")
                    c.raw_request(
                        "POST",
                        "/api/v2/torrents/removeTags",
                        body=f"hashes={tf.info_hash}&tags=t1".encode("utf-8"),
                        headers={"Content-Type": "application/x-www-form-urlencoded"},
                    )
                    self.assertEqual(qb.torrents[tf.info_hash]["tags"], "t2")
                    c.raw_request(
                        "POST",
                        "/api/v2/torrents/setDownloadLimit",
                        body=f"hashes={tf.info_hash}&limit=1000".encode("utf-8"),
                        headers={"Content-Type": "application/x-www-form-urlencoded"},
                    )
                    self.assertEqual(qb.torrents[tf.info_hash]["dl_limit"], 1000)

                    qb.drop_response_on_next_delete = True
                    with self.assertRaises((http.client.HTTPException, ConnectionResetError, ConnectionAbortedError, BrokenPipeError, EOFError)):
                        c.raw_request(
                            "POST",
                            "/api/v2/torrents/delete",
                            body=f"hashes={tf.info_hash}&deleteFiles=true".encode("utf-8"),
                            headers={"Content-Type": "application/x-www-form-urlencoded"},
                        )
                    self.assertNotIn(tf.info_hash, qb.torrents)
                    self.assertFalse((save_dir / "mock_ext_item.mkv").exists())

        with MockPtSiteServer() as pt:
            with ApiClient(pt.url) as c:
                self.assertEqual(
                    c.raw_request("GET", "/oversized-response").body.__len__(),
                    5 * 1024 * 1024 + 1024,
                )
                pt.webhook_fail_next = 1
                self.assertEqual(
                    c.raw_request("POST", "/webhook/events", body=b'{"id":"e1"}').status, 500
                )
                self.assertEqual(
                    c.raw_request("POST", "/webhook/events", body=b'{"id":"e1"}').status, 200
                )
                self.assertEqual(len(pt.webhook_events), 1)

                pt.drop_response_on_next_checkin = True
                with self.assertRaises((http.client.HTTPException, ConnectionResetError, ConnectionAbortedError, BrokenPipeError, EOFError)):
                    c.raw_request("POST", "/attendance.php")
                self.assertEqual(pt.checkin_post_count, 1)

        with MockObscuraCdpServer(required_cdp_token="cdp-tok-123") as cdp:
            with ApiClient(cdp.url) as c:
                self.assertEqual(
                    c.raw_request("GET", "/ws/browser").status,
                    401,
                )


class _ScopedAstVisitor(ast.NodeVisitor):
    """Collect undefined symbol loads within Python module ASTs."""

    def __init__(self, initial_scope: set) -> None:
        self.scopes = [set(initial_scope)]
        self.undef = []

    def _is_bound(self, name: str) -> bool:
        return any(name in s for s in reversed(self.scopes))

    def _bind_target(self, node: ast.AST) -> None:
        if isinstance(node, ast.Name):
            self.scopes[-1].add(node.id)
        elif isinstance(node, (ast.Tuple, ast.List)):
            for elt in node.elts:
                self._bind_target(elt)
        elif isinstance(node, ast.Starred):
            self._bind_target(node.value)

    def visit_Import(self, node: ast.Import) -> None:
        for alias in node.names:
            self.scopes[-1].add(alias.asname or alias.name.split(".", 1)[0])

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        for alias in node.names:
            if alias.name != "*":
                self.scopes[-1].add(alias.asname or alias.name)

    def visit_Assign(self, node: ast.Assign) -> None:
        self.visit(node.value)
        for t in node.targets:
            self._bind_target(t)

    def visit_AnnAssign(self, node: ast.AnnAssign) -> None:
        if node.value is not None:
            self.visit(node.value)
        self._bind_target(node.target)

    def visit_AugAssign(self, node: ast.AugAssign) -> None:
        self.visit(node.value)
        self.visit(node.target)

    def visit_For(self, node: ast.For) -> None:
        self.visit(node.iter)
        self._bind_target(node.target)
        for s in node.body:
            self.visit(s)
        for s in node.orelse:
            self.visit(s)

    def visit_With(self, node: ast.With) -> None:
        for item in node.items:
            self.visit(item.context_expr)
            if item.optional_vars is not None:
                self._bind_target(item.optional_vars)
        for s in node.body:
            self.visit(s)

    def visit_Try(self, node: ast.Try) -> None:
        for s in node.body:
            self.visit(s)
        for h in node.handlers:
            if h.type is not None:
                self.visit(h.type)
            if h.name:
                self.scopes[-1].add(h.name)
            for s in h.body:
                self.visit(s)
        for s in node.orelse:
            self.visit(s)
        for s in node.finalbody:
            self.visit(s)

    def visit_Module(self, node: ast.Module) -> None:
        for stmt in node.body:
            if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                self.scopes[-1].add(stmt.name)
        self.generic_visit(node)

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        self.scopes[-1].add(node.name)
        for dec in node.decorator_list:
            self.visit(dec)
        for base in node.bases:
            self.visit(base)
        for kw in node.keywords:
            self.visit(kw.value)
        self.scopes.append(set())
        for s in node.body:
            self.visit(s)
        self.scopes.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self.scopes[-1].add(node.name)
        for dec in node.decorator_list:
            self.visit(dec)
        for d in node.args.defaults:
            self.visit(d)
        for d in node.args.kw_defaults:
            if d is not None:
                self.visit(d)
        local = set()
        for a in list(node.args.posonlyargs) + list(node.args.args) + list(node.args.kwonlyargs):
            local.add(a.arg)
        if node.args.vararg:
            local.add(node.args.vararg.arg)
        if node.args.kwarg:
            local.add(node.args.kwarg.arg)
        # Pre-scan local definitions inside function body (Python function scope semantics)
        for sub in ast.walk(node):
            if sub is node:
                continue
            if isinstance(sub, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                if sub in node.body:
                    local.add(sub.name)
            elif isinstance(sub, ast.Name) and isinstance(sub.ctx, ast.Store):
                local.add(sub.id)
            elif isinstance(sub, ast.ExceptHandler) and sub.name:
                local.add(sub.name)
            elif isinstance(sub, (ast.Import, ast.ImportFrom)):
                for alias in sub.names:
                    local.add(alias.asname or alias.name.split(".", 1)[0])
        self.scopes.append(local)
        for s in node.body:
            self.visit(s)
        self.scopes.pop()

    def visit_Lambda(self, node: ast.Lambda) -> None:
        for d in node.args.defaults:
            self.visit(d)
        local = {
            a.arg
            for a in (list(node.args.posonlyargs) + list(node.args.args) + list(node.args.kwonlyargs))
        }
        if node.args.vararg:
            local.add(node.args.vararg.arg)
        if node.args.kwarg:
            local.add(node.args.kwarg.arg)
        self.scopes.append(local)
        self.visit(node.body)
        self.scopes.pop()

    def _visit_comp(self, generators: list, elt_nodes: list) -> None:
        local = set()
        self.scopes.append(local)
        for gen in generators:
            self.visit(gen.iter)
            self._bind_target(gen.target)
            for cond in gen.ifs:
                self.visit(cond)
        for e in elt_nodes:
            self.visit(e)
        self.scopes.pop()

    def visit_ListComp(self, node: ast.ListComp) -> None:
        self._visit_comp(node.generators, [node.elt])

    def visit_SetComp(self, node: ast.SetComp) -> None:
        self._visit_comp(node.generators, [node.elt])

    def visit_GeneratorExp(self, node: ast.GeneratorExp) -> None:
        self._visit_comp(node.generators, [node.elt])

    def visit_DictComp(self, node: ast.DictComp) -> None:
        self._visit_comp(node.generators, [node.key, node.value])

    def visit_Name(self, node: ast.Name) -> None:
        if isinstance(node.ctx, ast.Load) and not self._is_bound(node.id):
            self.undef.append((node.lineno, node.id))


def _extract_upload_pkg_info(content_type: str, body: bytes) -> tuple:
    if "boundary=" in content_type:
        boundary = content_type.split("boundary=", 1)[1].strip().strip('"').encode("latin-1")
        for part in body.split(b"--" + boundary):
            if b'name="file"' in part and b"\r\n\r\n" in part:
                payload = part.split(b"\r\n\r\n", 1)[1]
                if payload.endswith(b"\r\n"):
                    payload = payload[:-2]
                sha = hashlib.sha256(payload).hexdigest()
                pid = ""
                try:
                    with zipfile.ZipFile(io.BytesIO(payload), "r") as zf:
                        toml_txt = zf.read("plugin.toml").decode("utf-8", errors="ignore")
                        for line in toml_txt.splitlines():
                            if line.strip().startswith("id ="):
                                pid = line.split("=", 1)[1].strip().strip('"').strip("'")
                                break
                except Exception:
                    pid = ""
                return sha, pid
    return hashlib.sha256(body).hexdigest(), ""


def _make_baseline_handler():
    class BaselineHandler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        state_lock = threading.Lock()
        libs = {}
        subs = {}
        users = {
            "admin": {"id": "00000000-0000-0000-0000-000000000001", "login": "admin", "role": "admin", "enabled": True}
        }
        tokens = {}
        transfer_rev = 1
        watch_intake = None

        def log_message(self, format: str, *args: object) -> None:
            return

        def _read(self) -> bytes:
            n = int(self.headers.get("Content-Length", "0") or "0")
            return self.rfile.read(n) if n > 0 else b""

        def _send_json(self, status: int, payload: object, extra_headers: list = None) -> None:
            body = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            if extra_headers:
                for k, v in extra_headers:
                    self.send_header(k, v)
            self.end_headers()
            self.wfile.write(body)

        def _ok(self, data: object, status: int = 200, extra_headers: list = None) -> None:
            self._send_json(status, {"ok": True, "data": data}, extra_headers=extra_headers)

        def do_GET(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            if path == "/api/v1/health":
                self._ok({"status": "ok", "read_only": False})
            elif path == "/api/v1/auth/me":
                self._ok(self.users["admin"])
            elif path == "/api/v1/libraries":
                with self.state_lock:
                    self._ok(list(self.libs.values()))
            elif path.startswith("/api/v1/libraries/") and path.endswith("/items"):
                self._send_json(200, {"ok": True, "data": [], "next_cursor": None, "total": 0})
            elif path.startswith("/api/v1/libraries/") and "/scan-operations/" in path:
                op_id = path.rsplit("/", 1)[-1]
                self._ok({"operation_id": op_id, "status": "completed"})
            elif path.startswith("/api/v1/libraries/") and "/operations/" in path:
                op_id = path.rsplit("/", 1)[-1]
                self._ok({"operation_id": op_id, "state": "succeeded"})
            elif path == "/api/v1/downloaders/tasks":
                self._ok({"items": []})
            elif path.startswith("/api/v1/subscriptions/") and path.endswith("/activities"):
                self._ok({"items": [], "total": 0})
            elif path.startswith("/api/v1/subscriptions/"):
                sid = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    sub = self.subs.get(sid, {"id": sid, "library_id": None})
                self._ok(sub)
            elif path == "/api/v1/unidentified":
                self._ok([])
            elif path in ("/api/v1/playback/favorites", "/api/v1/playback/up-next"):
                self._ok({"items": []})
            elif path == "/api/v1/jobs":
                self._ok(
                    [
                        {"id": "00000000-0000-0000-0000-000000000010", "kind": "watch_intake", "name": "Watch intake", "enabled": True},
                        {"id": "00000000-0000-0000-0000-000000000011", "kind": "transfer", "name": "Transfer", "enabled": True},
                    ]
                )
            elif path == "/api/v1/settings/transfer":
                self._ok({"revision": self.transfer_rev, "watch_intake": self.watch_intake})
            elif path == "/api/v1/search/torrents":
                self._ok({"items": []})
            elif path == "/api/v1/sites/catalog":
                self._ok([{"profile_id": "pterclub", "auth_types": ["cookie"]}, {"profile_id": "mteam", "auth_types": ["apikey"]}])
            elif path == "/api/v1/jobs/stream":
                sse = b'event: job\ndata: {"id":"j1","state":"succeeded"}\n\n'
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(sse)))
                self.end_headers()
                self.wfile.write(sse)
            else:
                self._send_json(404, {"error": "not found"})

        def do_POST(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            raw = self._read()
            req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
            if path == "/api/v1/auth/login":
                tok = f"base-{uuid.uuid4().hex}"
                self._ok(
                    {"token": tok, "user": self.users["admin"]},
                    extra_headers=[("Set-Cookie", f"mc_session={tok}; Path=/; HttpOnly")],
                )
            elif path == "/api/v1/users":
                self._ok({"id": str(uuid.uuid4()), "login": req.get("login", "m"), "role": "member", "enabled": True}, status=201)
            elif path == "/api/v1/libraries":
                lid = str(uuid.uuid4())
                lib = {"id": lid, "name": req.get("name", "lib"), "kind": req.get("kind", "movie"), "root_paths": req.get("root_paths", [])}
                with self.state_lock:
                    self.libs[lid] = lib
                self._ok(lib, status=201)
            elif path.startswith("/api/v1/libraries/") and path.endswith("/scan"):
                self._ok({"operation_id": str(uuid.uuid4()), "status": "queued"}, status=202)
            elif path == "/api/v1/downloaders":
                self._ok({"id": str(uuid.uuid4()), "name": req.get("name", "qb")}, status=201)
            elif path == "/api/v1/sites":
                pid = req.get("profile_id", "")
                if pid not in ("pterclub", "mteam", "demo"):
                    self._send_json(400, {"ok": False, "error": {"code": "site.unknown_profile", "message": f"unknown profile {pid}"}})
                else:
                    self._ok({"id": str(uuid.uuid4()), "name": req.get("name", "site"), "profile_id": pid}, status=201)
            elif path == "/api/v1/subscriptions":
                sid = str(uuid.uuid4())
                sub = {"id": sid, "library_id": req.get("library_id")}
                with self.state_lock:
                    self.subs[sid] = sub
                self._ok(sub, status=201)
            elif path in ("/api/v1/downloaders/submit", "/api/v1/search/admit", "/api/v1/jobs/tick", "/api/v1/playback/marks") or (
                path.startswith("/api/v1/jobs/") and path.endswith("/run")
            ) or (
                path.startswith("/api/v1/subscriptions/") and path.endswith("/run")
            ) or (
                path.startswith("/api/v1/sites/") and path.endswith(("/check-in", "/verify"))
            ):
                self._ok({"ok": True, "accepted": True})
            else:
                self._send_json(404, {"error": "not found"})

        def do_PUT(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            raw = self._read()
            req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
            if path == "/api/v1/settings/browser":
                self._ok({"cdp": req.get("cdp", {"enabled": False})})
            elif path.startswith("/api/v1/libraries/") and path.endswith("/default"):
                self._ok({"default": True})
            else:
                self._send_json(404, {"error": "not found"})

        def do_PATCH(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            raw = self._read()
            req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
            if path == "/api/v1/settings/transfer":
                with self.state_lock:
                    self.transfer_rev += 1
                    if "watch_intake" in req:
                        self.watch_intake = req["watch_intake"]
                    self._ok({"revision": self.transfer_rev, "watch_intake": self.watch_intake})
            elif path.startswith("/api/v1/libraries/"):
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/sites/"):
                self._ok({"id": path.rsplit("/", 1)[-1]})
            else:
                self._send_json(404, {"error": "not found"})

        def do_DELETE(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            if path.startswith("/api/v1/libraries/"):
                lid = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    self.libs.pop(lid, None)
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/subscriptions/"):
                sid = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    self.subs.pop(sid, None)
                self._ok({"deleted": True})
            else:
                self._send_json(404, {"error": "not found"})

    return BaselineHandler


def _make_sweep_broken_handler(smart: bool):
    class SweepBrokenHandler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        state_lock = threading.Lock()
        smart_mode = smart
        libs = {}
        subs = {}
        sites = {}
        uploads = {}
        preps = {}
        ops = {}
        perm_consents = {}
        trust_keys = {}
        plugins = {
            "builtin.checkin": {
                "id": "builtin.checkin",
                "revision": 1,
                "config_revision": 1,
                "generation": 1,
                "lifecycle": "enabled",
                "enabled": True,
                "package_hash": "a" * 64,
            },
            "builtin.boost": {
                "id": "builtin.boost",
                "revision": 1,
                "config_revision": 1,
                "generation": 1,
                "lifecycle": "enabled",
                "enabled": True,
                "package_hash": "b" * 64,
            },
        }
        admin_user = {
            "id": "00000000-0000-0000-0000-000000000001",
            "login": "admin",
            "role": "admin",
            "enabled": True,
        }
        transfer_rev = 1
        watch_intake = None
        dev_mode = False
        dev_rev = 1

        def log_message(self, format: str, *args: object) -> None:
            return

        def _read(self) -> bytes:
            n = int(self.headers.get("Content-Length", "0") or "0")
            return self.rfile.read(n) if n > 0 else b""

        def _send_bytes(
            self, status: int, content_type: str, body: bytes, extra_headers: list = None
        ) -> None:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            if extra_headers:
                for k, v in extra_headers:
                    self.send_header(k, v)
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)

        def _send_json(self, status: int, payload: object, extra_headers: list = None) -> None:
            self._send_bytes(
                status,
                "application/json",
                json.dumps(payload).encode("utf-8"),
                extra_headers=extra_headers,
            )

        def _ok(self, data: object, status: int = 200, extra_headers: list = None) -> None:
            self._send_json(status, {"ok": True, "data": data}, extra_headers=extra_headers)

        def _handle_asset(self) -> None:
            self._send_bytes(
                200,
                "application/javascript",
                b"/* broken asset stub */\n",
                extra_headers=[
                    ("X-Content-Type-Options", "nosniff"),
                    ("Cache-Control", "private, no-store"),
                ],
            )

        def do_HEAD(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            if path.startswith("/api/v1/plugin-ui-assets/"):
                self._handle_asset()
            else:
                self._send_bytes(404, "application/json", b"")

        def do_GET(self) -> None:
            parsed = urllib.parse.urlsplit(self.path)
            path = parsed.path
            if path == "/api/v1/health":
                self._ok({"status": "ok", "read_only": False})
            elif path == "/api/v1/auth/me":
                self._ok(self.admin_user)
            elif path == "/api/v1/settings/plugins":
                self._ok({"developer_mode": self.dev_mode, "revision": self.dev_rev})
            elif path == "/api/v1/settings/transfer":
                self._ok({"revision": self.transfer_rev, "watch_intake": self.watch_intake})
            elif path == "/api/v1/settings/browser":
                self._ok({"cdp": {"enabled": False}})
            elif path == "/api/v1/plugin-trust-keys":
                with self.state_lock:
                    self._ok(list(self.trust_keys.values()))
            elif path in ("/api/v1/plugin-hook-catalog", "/api/v1/plugin-repos", "/api/v1/plugin-ui-navigation"):
                self._ok([])
            elif path == "/api/v1/plugin-ui-contract":
                if self.smart_mode:
                    self._ok({"contract": "1.0.0", "react": "19.3.0", "react_dom": "19.3.0", "sdk_methods": ["query"]})
                else:
                    self._ok({"contract": "0.0.0", "react": "0.0.0"})
            elif path.startswith("/api/v1/plugin-ui-assets/"):
                self._handle_asset()
            elif path.startswith("/api/v1/plugin-operations/"):
                op_id = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    op = self.ops.get(op_id)
                if op is None:
                    op = {
                        "operation_id": op_id,
                        "state": "succeeded",
                        "result": {
                            "preparation_id": f"prep-{op_id}",
                            "consent_token": f"consent-{op_id}",
                            "grants": [],
                            "plan_handle": f"plan-{op_id}",
                        },
                    }
                self._ok(op)
            elif path == "/api/v1/plugins":
                with self.state_lock:
                    self._ok(list(self.plugins.values()))
            elif path.startswith("/api/v1/plugins/") and path.endswith("/config"):
                self._ok({"schema_revision": "1", "revision": 1, "config_revision": 1, "values": {}, "secrets": {}})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/config-schema"):
                self._ok({"schema_revision": "1", "schema": {"type": "object"}})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions"):
                self._ok({"effective_grants": [], "grants": []})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/tasks"):
                self._ok([])
            elif path.startswith("/api/v1/plugins/") and "/tasks/" in path and path.endswith("/receipts"):
                self._ok([])
            elif path.startswith("/api/v1/plugins/") and path.endswith("/audit"):
                pid = path.split("/")[4]
                self._ok(
                    [
                        {"plugin_id": pid, "action": "plugin.install"},
                        {"plugin_id": pid, "action": "plugin.enable"},
                    ]
                )
            elif path.startswith("/api/v1/plugins/") and path.endswith("/brush-state"):
                self._ok(
                    {
                        "used_bytes": 0,
                        "reserved_bytes": 0,
                        "retained_file_bytes": 0,
                        "tasks": [],
                        "disk": {"status": "available", "available_bytes": 0},
                    }
                )
            elif path.startswith("/api/v1/plugins/") and path.endswith("/brush-cleanup-preview"):
                self._ok({"mode": "selection", "confirmation_token": None})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/browser-availability"):
                if self.smart_mode:
                    self._ok({"status": "unavailable", "missing_requirements": ["browser"]})
                else:
                    self._ok({"status": "unavailable"})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/ui"):
                pid = path.split("/")[4]
                with self.state_lock:
                    p = self.plugins.get(pid, {})
                self._ok(
                    {
                        "subject_user_id": self.admin_user["id"],
                        "plugin_id": pid,
                        "package_hash": p.get("package_hash", "0" * 64),
                        "entry_url": f"/api/v1/plugin-ui-assets/{pid}/{p.get('package_hash', '0' * 64)}/ui/assets/remoteEntry.js",
                        "contract": "1.0.0",
                        "react": "19.3.0",
                        "surfaces": (
                            [{"id": "cfg", "kind": "config", "editable": True, "mountable": True}]
                            if self.smart_mode
                            else []
                        ),
                        "trust": {"status": "untrusted", "revision": 0},
                    }
                )
            elif path.startswith("/api/v1/plugins/") and path.endswith("/endpoint-keys"):
                self._ok([])
            elif path.startswith("/api/v1/plugins/") and path.endswith("/metrics"):
                self._ok({"total_invocations": 0})
            elif path.startswith("/api/v1/plugins/") and "/endpoints/" in path:
                self._ok({"ok": True})
            elif path.startswith("/api/v1/plugins/"):
                pid = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    p = self.plugins.get(
                        pid,
                        {
                            "id": pid,
                            "revision": 1,
                            "config_revision": 1,
                            "generation": 1 if self.smart_mode else 0,
                            "lifecycle": "enabled",
                            "enabled": True,
                            "package_hash": "0" * 64,
                        },
                    )
                self._ok(p)
            elif path == "/api/v1/libraries":
                with self.state_lock:
                    self._ok(list(self.libs.values()))
            elif path.startswith("/api/v1/libraries/") and (path.endswith("/items") or path.endswith("/files")):
                self._send_json(200, {"ok": True, "data": [], "next_cursor": None, "total": 0})
            elif path.startswith("/api/v1/libraries/") and "/items/" in path:
                self._ok({"media_item_id": path.rsplit("/", 1)[-1], "files": []})
            elif path.startswith("/api/v1/libraries/") and ("/operations/" in path or "/scan-operations/" in path):
                op_id = path.rsplit("/", 1)[-1]
                self._ok({"operation_id": op_id, "state": "succeeded", "status": "completed"})
            elif path.startswith("/api/v1/library-removals/"):
                op_id = path.rsplit("/", 1)[-1]
                self._ok({"operation_id": op_id, "state": "succeeded"})
            elif path == "/api/v1/jobs":
                self._ok(
                    [
                        {"id": "00000000-0000-0000-0000-000000000010", "kind": "watch_intake", "name": "Watch", "enabled": True},
                        {"id": "00000000-0000-0000-0000-000000000011", "kind": "transfer", "name": "Transfer", "enabled": True},
                    ]
                )
            elif path == "/api/v1/jobs/stream":
                sse = b'event: job\ndata: {"id":"j1","state":"succeeded"}\n\n'
                self._send_bytes(200, "text/event-stream", sse)
            elif path == "/api/v1/downloaders":
                self._ok([])
            elif path == "/api/v1/downloaders/tasks":
                self._ok({"items": []})
            elif path == "/api/v1/sites":
                with self.state_lock:
                    self._ok(list(self.sites.values()) if self.smart_mode else [])
            elif path == "/api/v1/sites/catalog":
                self._ok([])
            elif path.startswith("/api/v1/sites/") and path.endswith("/plugin-status"):
                self._ok({"status": "idle", "receipts": []})
            elif path == "/api/v1/search/torrents":
                self._ok({"items": [{"id": "stub-1", "title": "Stub Search Item"}]})
            elif path.startswith("/api/v1/subscriptions/") and path.endswith("/activities"):
                self._ok({"items": [], "total": 0})
            elif path.startswith("/api/v1/subscriptions/"):
                sid = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    self._ok(self.subs.get(sid, {"id": sid, "library_id": None}))
            elif path in ("/api/v1/playback/favorites", "/api/v1/playback/up-next"):
                self._ok({"items": []})
            elif path == "/api/v1/unidentified":
                self._ok([])
            elif path.startswith("/plugins/"):
                self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})
            elif path.startswith("/plugin-apps/"):
                self._send_bytes(200, "text/html; charset=utf-8", b"<html><body>stub</body></html>")
            else:
                self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        def do_POST(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            raw = self._read()
            req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
            if path == "/api/v1/auth/login":
                tok = f"broken-{uuid.uuid4().hex}"
                self._ok(
                    {"token": tok, "user": self.admin_user},
                    extra_headers=[("Set-Cookie", f"mc_session={tok}; Path=/; HttpOnly")],
                )
            elif path == "/api/v1/auth/logout":
                self._ok({"logged_out": True})
            elif path == "/api/v1/users":
                self._ok({"id": str(uuid.uuid4()), "login": req.get("login", "member"), "role": "member"}, status=201)
            elif path == "/api/v1/plugin-reauth":
                purpose = req.get("purpose")
                if purpose not in VALID_REAUTH_PURPOSES:
                    self._send_json(400, {"ok": False, "error": {"code": "auth.invalid", "message": f"invalid purpose: {purpose!r}"}})
                    return
                if self.smart_mode:
                    if req.get("password") != "e2e-admin-password-secret":
                        self._send_json(401, {"ok": False, "error": {"code": "auth.invalid", "message": "Invalid credentials"}})
                        return
                    self._ok({"reauth_token": f"reauth-{uuid.uuid4().hex}", "purpose": purpose, "expires_at": "2099-01-01T00:00:00Z"})
                else:
                    self._ok({"reauth_token": f"reauth-{uuid.uuid4().hex}"})
            elif path == "/api/v1/plugin-trust-keys/prepare":
                self._ok({"key_id": "0" * 16, "fingerprint": "f" * 64, "consent_token": "c-trust", "expires_at": "2099-01-01T00:00:00Z"})
            elif path == "/api/v1/plugin-trust-keys":
                kid = f"k-{uuid.uuid4().hex[:8]}"
                row = {"id": str(uuid.uuid4()), "key_id": kid, "status": "trusted", "revision": 1, "fingerprint": req.get("fingerprint", "f" * 64)}
                with self.state_lock:
                    self.trust_keys[kid] = row
                self._ok(row, status=201)
            elif path.startswith("/api/v1/plugin-trust-keys/") and path.endswith("/revoke"):
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path == "/api/v1/plugin-uploads":
                uid = str(uuid.uuid4())
                if self.smart_mode:
                    h, pid = _extract_upload_pkg_info(self.headers.get("Content-Type", ""), raw)
                else:
                    h, pid = "0" * 64, ""
                with self.state_lock:
                    self.uploads[uid] = (h, pid)
                self._ok({"upload_id": uid, "hash": h, "signature_status": "signed" if self.smart_mode else "unknown"}, status=201)
            elif path == "/api/v1/plugin-installations/prepare":
                op_id = str(uuid.uuid4())
                prep_id = f"prep-{op_id}"
                uid = req.get("upload_id", "")
                with self.state_lock:
                    h, pid = self.uploads.get(uid, ("0" * 64, ""))
                    self.preps[prep_id] = (h, pid)
                    self.ops[op_id] = {
                        "operation_id": op_id,
                        "state": "succeeded",
                        "result": {
                            "preparation_id": prep_id,
                            "consent_token": f"consent-{op_id}",
                            "package_hash": h if self.smart_mode else "0" * 64,
                            "grants": [],
                        },
                    }
                self._ok({"operation_id": op_id, "state": "queued"}, status=202)
            elif path == "/api/v1/plugin-installations":
                if self.smart_mode:
                    prep_id = req.get("preparation_id", "")
                    with self.state_lock:
                        h, pid = self.preps.get(prep_id, ("0" * 64, ""))
                        if pid:
                            self.plugins[pid] = {
                                "id": pid,
                                "revision": 1,
                                "config_revision": 1,
                                "generation": 0,
                                "lifecycle": "installed-disabled",
                                "enabled": False,
                                "package_hash": h,
                            }
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/plugins/") and path.endswith("/enable"):
                if self.smart_mode:
                    pid = path.split("/")[4]
                    with self.state_lock:
                        cur = self.plugins.setdefault(
                            pid,
                            {"id": pid, "revision": 1, "config_revision": 1, "generation": 0, "package_hash": "0" * 64},
                        )
                        cur["lifecycle"] = "enabled"
                        cur["enabled"] = True
                        cur["generation"] = int(cur.get("generation", 0)) + 1
                        cur["revision"] = int(cur.get("revision", 1)) + 1
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/plugins/") and path.endswith("/disable"):
                if self.smart_mode:
                    pid = path.split("/")[4]
                    with self.state_lock:
                        cur = self.plugins.get(pid)
                        if cur is not None:
                            cur["lifecycle"] = "disabled"
                            cur["enabled"] = False
                            cur["generation"] = int(cur.get("generation", 0)) + 1
                            cur["revision"] = int(cur.get("revision", 1)) + 1
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/plugins/") and path.endswith("/ui/query"):
                self._ok({"items": []})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/config/validate"):
                self._ok({"valid": True})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions/prepare"):
                req_grants = req.get("requested_grants", [])
                ctok = f"c-perm-{uuid.uuid4().hex}"
                digest = hashlib.sha256(
                    json.dumps(
                        sorted(req_grants, key=lambda x: json.dumps(x, sort_keys=True)),
                        sort_keys=True,
                    ).encode("utf-8")
                ).hexdigest()
                with self.state_lock:
                    self.perm_consents[ctok] = digest
                self._ok({"consent_token": ctok, "grants": req_grants})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions/grant"):
                if "reauth_token" in req:
                    self._send_json(400, {"ok": False, "error": {"code": "plugin.invalid_input", "message": "unexpected reauth_token"}})
                    return
                if self.smart_mode:
                    ctok = req.get("consent_token", "")
                    sub_grants = req.get("grants", [])
                    digest = hashlib.sha256(
                        json.dumps(
                            sorted(sub_grants, key=lambda x: json.dumps(x, sort_keys=True)),
                            sort_keys=True,
                        ).encode("utf-8")
                    ).hexdigest()
                    with self.state_lock:
                        exp_digest = self.perm_consents.get(ctok)
                    if exp_digest is not None and exp_digest != digest:
                        self._send_json(
                            409,
                            {"ok": False, "error": {"code": "plugin.consent_stale", "message": "grants digest mismatch"}},
                        )
                        return
                self._ok({"granted": True})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions/revoke"):
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/plugins/") and path.endswith("/ui-trust/prepare"):
                self._ok({"consent_token": "c-uitrust", "trust_revision": 0})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/ui-trust/grant"):
                self._ok({"status": "trusted", "revision": 1})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/ui-trust/revoke"):
                self._ok({"status": "revoked", "revision": 2})
            elif path.startswith("/api/v1/plugins/") and "/tasks/" in path and path.endswith(("/run", "/update")):
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/plugins/") and path.endswith(
                ("/uninstall", "/browser-checks", "/commands", "/upgrades/prepare", "/upgrade", "/rollback", "/state/reset")
            ) or (path.startswith("/api/v1/sites/") and path.endswith("/plugin-actions")):
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/plugins/") and path.endswith("/state/reset/prepare"):
                self._ok({"consent_token": "c-reset", "kv_entries": 0})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/endpoint-keys"):
                self._ok({"id": str(uuid.uuid4())}, status=201)
            elif path.startswith("/api/v1/plugin-operations/") and path.endswith("/cancel"):
                op_id = path.split("/")[-2]
                self._ok({"operation_id": op_id, "state": "cancelled"}, status=202)
            elif path.startswith("/api/v1/plugin-operations/") and path.endswith("/resume"):
                op_id = path.split("/")[-2]
                self._ok({"operation_id": op_id, "state": "succeeded"}, status=202)
            elif path == "/api/v1/plugin-repos":
                self._ok({"id": req.get("id", "r1"), "revision": 1}, status=201)
            elif path.startswith("/api/v1/settings/plugins/developer/"):
                en = path.endswith("/enable")
                if not en and "reauth_token" in req:
                    self._send_json(400, {"ok": False, "error": {"code": "plugin.invalid_input", "message": "disable takes no reauth_token"}})
                    return
                with self.state_lock:
                    self.dev_mode = en
                    self.dev_rev += 1
                self._ok({"developer_mode": en, "revision": self.dev_rev})
            elif path == "/api/v1/libraries":
                lid = str(uuid.uuid4())
                lib = {"id": lid, "name": req.get("name", "lib"), "kind": req.get("kind", "movie"), "root_paths": req.get("root_paths", [])}
                with self.state_lock:
                    self.libs[lid] = lib
                self._ok(lib, status=201)
            elif path.startswith("/api/v1/libraries/") and path.endswith("/scan"):
                self._ok({"operation_id": str(uuid.uuid4()), "status": "queued"}, status=202)
            elif path == "/api/v1/downloaders":
                self._ok({"id": str(uuid.uuid4()), "name": req.get("name", "qb")}, status=201)
            elif path == "/api/v1/sites":
                sid = str(uuid.uuid4())
                s = {"id": sid, "name": req.get("name", "site"), "profile_id": req.get("profile_id", "pterclub")}
                with self.state_lock:
                    self.sites[sid] = s
                self._ok(s, status=201)
            elif path == "/api/v1/subscriptions":
                sid = str(uuid.uuid4())
                sub = {"id": sid, "library_id": req.get("library_id")}
                with self.state_lock:
                    self.subs[sid] = sub
                self._ok(sub, status=201)
            elif path == "/api/v1/jobs/tick":
                self._ok({"ran": 0, "kinds": [], "skipped_maintenance": []})
            elif path.startswith("/api/v1/jobs/") and path.endswith("/run"):
                self._ok({"status": "created", "queued": True})
            elif path in ("/api/v1/downloaders/submit", "/api/v1/search/admit", "/api/v1/playback/marks") or (
                path.startswith("/api/v1/subscriptions/") and path.endswith("/run")
            ) or (
                path.startswith("/api/v1/sites/") and path.endswith(("/check-in", "/verify"))
            ):
                self._ok({"ok": True, "accepted": True})
            else:
                self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        def do_PUT(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            raw = self._read()
            req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
            if path == "/api/v1/settings/browser":
                self._ok({"cdp": req.get("cdp", {"enabled": False})})
            elif path.startswith("/api/v1/libraries/") and path.endswith("/default"):
                self._ok({"default": True})
            elif path.startswith("/api/v1/plugins/") and path.endswith("/config"):
                vals = req.get("values") or {}
                if self.smart_mode and isinstance(vals, dict) and isinstance(vals.get("site_id"), str):
                    try:
                        sid_norm = str(uuid.UUID(vals["site_id"]))
                    except ValueError:
                        self._send_json(
                            400,
                            {"ok": False, "error": {"code": "plugin.scope_invalid", "message": "invalid site_id UUID"}},
                        )
                        return
                    with self.state_lock:
                        if sid_norm not in self.sites:
                            self._send_json(
                                400,
                                {"ok": False, "error": {"code": "plugin.scope_invalid", "message": "referenced site_id does not exist"}},
                            )
                            return
                self._ok(
                    {
                        "revision": 2,
                        "config_revision": 2,
                        "schema_revision": "1",
                        "values": vals,
                        "secrets": {"api_token": {"configured": True}},
                    }
                )
            else:
                self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        def do_PATCH(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            raw = self._read()
            req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
            if path == "/api/v1/settings/transfer":
                with self.state_lock:
                    self.transfer_rev += 1
                    if "watch_intake" in req:
                        self.watch_intake = req["watch_intake"]
                    self._ok({"revision": self.transfer_rev, "watch_intake": self.watch_intake})
            elif path.startswith("/api/v1/libraries/"):
                lid = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    if lid in self.libs and "root_paths" in req:
                        self.libs[lid]["root_paths"] = list(req["root_paths"])
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/sites/"):
                self._ok({"id": path.rsplit("/", 1)[-1]})
            else:
                self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        def do_DELETE(self) -> None:
            path = urllib.parse.urlsplit(self.path).path
            if path.startswith("/api/v1/libraries/"):
                lid = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    self.libs.pop(lid, None)
                self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
            elif path.startswith("/api/v1/subscriptions/"):
                sid = path.rsplit("/", 1)[-1]
                with self.state_lock:
                    self.subs.pop(sid, None)
                self._ok({"deleted": True})
            else:
                self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

    return SweepBrokenHandler


class TestIteration2IntegrityAndRegressions(unittest.TestCase):
    def test_static_ast_guards_across_tiers(self) -> None:
        tiers_dir = Path(__file__).resolve().parents[1] / "tiers"
        tier_files = sorted(tiers_dir.glob("*.py"))
        builtin_names = set(dir(builtins))

        for path in tier_files:
            src = path.read_text(encoding="utf-8")
            tree = ast.parse(src, filename=str(path))
            visitor = _ScopedAstVisitor(builtin_names)
            visitor.visit(tree)
            self.assertEqual(visitor.undef, [], f"Undefined symbols in {path.name}: {visitor.undef}")

            for node in ast.walk(tree):
                if isinstance(node, ast.ExceptHandler):
                    if node.type is None or (
                        isinstance(node.type, ast.Name) and node.type.id in ("Exception", "BaseException")
                    ):
                        self.fail(f"Forbidden broad except handler in {path.name}:{node.lineno}")
                if isinstance(node, ast.BoolOp) and isinstance(node.op, ast.Or):
                    for v in node.values:
                        if isinstance(v, ast.Constant) and v.value is True:
                            self.fail(f"Tautological `or True` in {path.name}:{node.lineno}")
                if isinstance(node, ast.Compare) and len(node.ops) == 1 and isinstance(node.ops[0], ast.GtE):
                    comp = node.comparators[0]
                    if isinstance(comp, ast.Constant) and comp.value == 0:
                        if isinstance(node.left, ast.Call) and isinstance(node.left.func, ast.Name) and node.left.func.id == "len":
                            self.fail(f"Tautological `len(...) >= 0` in {path.name}:{node.lineno}")
                        if "retained_file_bytes" in ast.dump(node.left):
                            self.fail(f"Vacuous `retained_file_bytes >= 0` in {path.name}:{node.lineno}")
                if isinstance(node, ast.Constant) and isinstance(node.value, str):
                    if node.value in ("base_url", "enclosure_url"):
                        self.fail(f"Forbidden legacy field {node.value!r} in {path.name}:{node.lineno}")
                if isinstance(node, ast.Attribute):
                    if node.attr == "self_test" and isinstance(node.value, ast.Name) and node.value.id == "ctx":
                        self.fail(f"Forbidden `ctx.self_test` branch in {path.name}:{node.lineno}")

    def test_realistic_baseline_404_probe_zero_fake_passes_and_zero_name_errors(self) -> None:
        suite_ref = self
        BaselineHandler = _make_baseline_handler()

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), BaselineHandler)
        thr = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thr.start()
        try:
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    url = f"http://127.0.0.1:{srv.server_address[1]}"
                    args = parse_args(["--target-url", url, "--tier", "1", "--tier", "2"])
                    report = run_suite(args, registry=build_default_registry())
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root

            summary = report["summary"]
            suite_ref.assertGreaterEqual(summary["total"], 264)
            suite_ref.assertEqual(
                summary["passed"],
                0,
                f"Fake-passing baseline cases: {[r['id'] for r in report['results'] if r['status'] == 'pass']}",
            )
            suite_ref.assertEqual(summary["failed"], summary["total"])
            for r in report["results"]:
                err_txt = r.get("error") or ""
                for bad_exc in ("NameError", "AttributeError", "TypeError", "KeyError"):
                    suite_ref.assertNotIn(bad_exc, err_txt, f"Case {r['id']} crashed with {bad_exc}: {err_txt}")
                suite_ref.assertTrue(
                    r["unimplemented"],
                    f"Case {r['id']} did not report unimplemented=True on baseline 404 server: {err_txt}",
                )
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_restart_server_and_run_suite_port_sync(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            bin_script = Path(td) / "fake_serve.py"
            bin_script.write_text(
                "\n".join(
                    [
                        "#!" + sys.executable,
                        "import http.server, json, os, sys, urllib.parse",
                        "port = int(os.environ['CRAWLER_MEDIA_LISTEN'].rsplit(':', 1)[1])",
                        "class H(http.server.BaseHTTPRequestHandler):",
                        "    protocol_version = 'HTTP/1.1'",
                        "    def log_message(self, *a): return",
                        "    def _send(self, st, payload):",
                        "        b = json.dumps({'ok': True, 'data': payload}).encode()",
                        "        self.send_response(st); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)",
                        "    def do_GET(self):",
                        "        p = urllib.parse.urlsplit(self.path).path",
                        "        if p == '/api/v1/health': self._send(200, {'status': 'ok', 'read_only': False})",
                        "        elif p == '/api/v1/auth/me': self._send(200, {'id': 'u1', 'login': 'admin', 'role': 'admin'})",
                        "        else: self._send(200, {'port': port})",
                        "    def do_POST(self):",
                        "        n = int(self.headers.get('Content-Length', '0') or '0')",
                        "        if n: self.rfile.read(n)",
                        "        self._send(200, {'token': f'tok-{port}', 'user': {'id': 'u1', 'role': 'admin'}})",
                        "http.server.HTTPServer(('127.0.0.1', port), H).serve_forever()",
                    ]
                ),
                encoding="utf-8",
            )
            os.chmod(bin_script, 0o755)
            reg = SuiteRegistry()

            @reg.case(id="RST-01", tier=1, feature_id=5, name="Restart updates ctx and run_suite port", self_test=False)
            def case_rst_1(ctx: TestContext) -> None:
                old_url = ctx.base_url
                new_url = ctx.restart_server()
                self.assertNotEqual(old_url, new_url)
                me = ctx.client.me()
                self.assertEqual(me["role"], "admin")
                self.assertEqual(ctx.new_client().base_url, new_url)

            @reg.case(id="RST-02", tier=1, feature_id=5, name="Next test uses updated live port", self_test=False)
            def case_rst_2(ctx: TestContext) -> None:
                self.assertEqual(ctx.client.me()["role"], "admin")

            prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
            os.environ["CRAWLER_E2E_TMP_ROOT"] = td
            try:
                args = parse_args(["--spawn-backend", "--bin", str(bin_script), "--tier", "1"])
                report = run_suite(args, registry=reg)
            finally:
                if prev_root is None:
                    os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                else:
                    os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root

            self.assertEqual(report["summary"]["total"], 2)
            self.assertEqual(report["summary"]["passed"], 2)
            self.assertEqual(report["summary"]["failed"], 0)

    def test_run_plugin_cli_and_assert_api_error_404_classification(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            unimpl_bin = Path(td) / "cli_unimpl.sh"
            unimpl_bin.write_text("#!/bin/sh\necho 'unknown command plugin' >&2\nexit 2\n", encoding="utf-8")
            os.chmod(unimpl_bin, 0o755)

            err_bin = Path(td) / "cli_err.sh"
            err_bin.write_text("#!/bin/sh\necho 'invalid manifest' >&2\nexit 1\n", encoding="utf-8")
            os.chmod(err_bin, 0o755)

            ctx = TestContext(
                base_url="http://127.0.0.1:1",
                client=ApiClient("http://127.0.0.1:1"),
                admin_password="p",
                cli_token="t",
                bin_path=unimpl_bin,
            )
            try:
                with self.assertRaises(UnimplementedFeatureError):
                    run_plugin_cli(ctx, ["validate", td])

                ctx.bin_path = err_bin
                proc = run_plugin_cli(ctx, ["validate", td])
                self.assertEqual(proc.returncode, 1)
            finally:
                ctx.close()

        # assert_api_error with 404 in expected_statuses
        with self.assertRaises(UnimplementedFeatureError):
            assert_api_error(
                lambda: (_ for _ in ()).throw(
                    UnimplementedFeatureError("GET /missing")
                ),
                (404, 409),
                label="unmatched without cause",
            )
        exc_none = ApiError(404, "/missing", None, "not found")
        with self.assertRaises(UnimplementedFeatureError):
            try:
                raise UnimplementedFeatureError("GET /missing") from exc_none
            except UnimplementedFeatureError as u:
                assert_api_error(lambda err=u: (_ for _ in ()).throw(err), (404, 409))

        exc_stub_404 = ApiError(404, "/missing", "not_found", "No stub route")
        with self.assertRaises(UnimplementedFeatureError):
            assert_api_error(lambda: (_ for _ in ()).throw(exc_stub_404), (404, 409))

        exc_domain = ApiError(404, "/libraries/x/items", "library.missing", "媒体库不存在")
        try:
            raise UnimplementedFeatureError("GET /libraries/x/items") from exc_domain
        except UnimplementedFeatureError as u:
            got = assert_api_error(lambda err=u: (_ for _ in ()).throw(err), (404, 409))
            self.assertEqual(got.code, "library.missing")

    def test_mock_qbit_save_dir_sanitization_obscura_404_and_unsupported_hook_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            save_dir = Path(td) / "qb_safe"
            with MockQbitServer(require_auth=False, real_save_dir=save_dir) as qb:
                with ApiClient(qb.url) as c:
                    r1 = c.raw_request(
                        "POST",
                        "/api/v2/torrents/add",
                        body=urllib.parse.urlencode(
                            {"urls": "http://127.0.0.1/test.torrent", "savepath": "/downloads/boost"}
                        ).encode("utf-8"),
                        headers={"Content-Type": "application/x-www-form-urlencoded"},
                    )
                    self.assertEqual(r1.status, 200)
                    created_files = list(save_dir.rglob("*.mkv"))
                    self.assertEqual(len(created_files), 1)
                    self.assertTrue(str(created_files[0].resolve()).startswith(str((save_dir / "boost").resolve())))

                    qb.drop_response_on_next_add = True
                    with self.assertRaises((http.client.HTTPException, ConnectionResetError, ConnectionAbortedError, BrokenPipeError, EOFError)):
                        c.raw_request(
                            "POST",
                            "/api/v2/torrents/add",
                            body=urllib.parse.urlencode(
                                {"urls": "http://127.0.0.1/escape.torrent", "savepath": "../../escape"}
                            ).encode("utf-8"),
                            headers={"Content-Type": "application/x-www-form-urlencoded"},
                        )
                    self.assertFalse(qb.drop_response_on_next_add)
                    escape_files = list((save_dir / "escape").glob("*.mkv"))
                    self.assertEqual(len(escape_files), 1)

                    hashes = "|".join(qb.torrents.keys())
                    del_r = c.raw_request(
                        "POST",
                        "/api/v2/torrents/delete",
                        body=f"hashes={hashes}&deleteFiles=true".encode("utf-8"),
                        headers={"Content-Type": "application/x-www-form-urlencoded"},
                    )
                    self.assertEqual(del_r.status, 200)
                    self.assertEqual(list(save_dir.rglob("*.mkv")), [])

        with MockObscuraCdpServer(required_cdp_token="tok") as cdp:
            parts = urllib.parse.urlsplit(cdp.url)
            for path, expected_status in (
                ("/nonexistent-cdp-path", 404),
                ("/ws/browser", 401),
            ):
                conn = http.client.HTTPConnection(parts.hostname, parts.port, timeout=1.0)
                try:
                    conn.request("GET", path)
                    resp = conn.getresponse()
                    self.assertEqual(resp.status, expected_status)
                    resp.read()
                finally:
                    conn.close()

        with MockObscuraCdpServer() as cdp_noauth:
            parts = urllib.parse.urlsplit(cdp_noauth.url)
            conn = http.client.HTTPConnection(parts.hostname, parts.port, timeout=1.0)
            try:
                conn.request("GET", "/ws/browser")
                resp = conn.getresponse()
                self.assertEqual(resp.status, 400)
                resp.read()
            finally:
                conn.close()

        raw_bad_hook = build_negative_cmplugin("invalid_toml_unsupported_hook")
        with zipfile.ZipFile(io.BytesIO(raw_bad_hook), "r") as zf:
            toml_txt = zf.read("plugin.toml").decode("utf-8")
            self.assertIn('permissions = ["hooks:search.after"]', toml_txt)
            self.assertIn('id = "playback.progress"', toml_txt)

    def test_broken_phase1_fault_injection_zero_vacuous_passes(self) -> None:
        suite_ref = self
        hardened_ids = (
            "T1-F12-02",
            "T1-F14-02",
            "T2-F13-05",
            "T2-F14-02",
            "T2-F16-01",
            "T2-F16-05",
            "T1-F19-01",
            "T1-F19-04",
            "T2-F19-03",
            "T2-F19-04",
            "T1-F21-04",
            "T1-F21-05",
            "T2-F14-04",
            "T1-F16-05",
            "T1-F18-01",
            "T1-F06-05",
            "T1-F11-04",
            "T1-F01-03",
            "T1-F01-04",
            "T1-F01-05",
            "T1-F02-02",
            "T1-F03-01",
            "T1-F22-02",
            "T1-F23-02",
            "T1-F23-03",
        )

        class BrokenPhase1Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            state_lock = threading.Lock()
            libs = {}
            subs = {}
            plugins = {
                "builtin.checkin": {"id": "builtin.checkin", "revision": 1, "lifecycle": "enabled"},
                "builtin.boost": {"id": "builtin.boost", "revision": 1, "lifecycle": "enabled"},
            }
            admin_user = {"id": "00000000-0000-0000-0000-000000000001", "login": "admin", "role": "admin", "enabled": True}
            transfer_rev = 1
            watch_intake = None

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _send_json(self, status: int, payload: object, extra_headers: list = None) -> None:
                body = json.dumps(payload).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def _ok(self, data: object, status: int = 200, extra_headers: list = None) -> None:
                self._send_json(status, {"ok": True, "data": data}, extra_headers=extra_headers)

            def do_GET(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                if path == "/api/v1/health":
                    self._ok({"status": "ok", "read_only": False})
                elif path == "/api/v1/auth/me":
                    self._ok(self.admin_user)
                elif path == "/api/v1/libraries":
                    with self.state_lock:
                        self._ok(list(self.libs.values()))
                elif path.startswith("/api/v1/libraries/") and path.endswith("/items"):
                    self._send_json(200, {"ok": True, "data": [], "next_cursor": None, "total": 0})
                elif path.startswith("/api/v1/libraries/") and "/operations/" in path:
                    op_id = path.rsplit("/", 1)[-1]
                    self._ok({"operation_id": op_id, "state": "succeeded"})
                elif path == "/api/v1/jobs":
                    self._ok(
                        [
                            {"id": "00000000-0000-0000-0000-000000000010", "kind": "watch_intake", "name": "Watch intake", "enabled": True},
                            {"id": "00000000-0000-0000-0000-000000000011", "kind": "transfer", "name": "Transfer", "enabled": True},
                        ]
                    )
                elif path == "/api/v1/settings/transfer":
                    self._ok({"revision": self.transfer_rev, "watch_intake": self.watch_intake})
                elif path == "/api/v1/search/torrents":
                    # Non-empty synthetic search item, but never hit MockPtSiteServer or search.before hook
                    self._ok({"items": [{"id": "stub-1", "title": "Stub Search Item"}]})
                elif path.startswith("/api/v1/subscriptions/") and path.endswith("/activities"):
                    self._ok({"items": [], "total": 0})
                elif path == "/api/v1/sites":
                    # Broken no-op: returns empty site list
                    self._ok([])
                elif path.startswith("/api/v1/sites/") and path.endswith("/plugin-status"):
                    self._ok({"status": "idle", "receipts": []})
                elif path == "/api/v1/plugins":
                    with self.state_lock:
                        self._ok(list(self.plugins.values()))
                elif path.startswith("/api/v1/plugins/") and path.endswith("/audit"):
                    # Install/enable rows exist, but zero hook/boost execution rows
                    self._ok([{"action": "plugin.install"}, {"action": "plugin.enable"}])
                elif path.startswith("/api/v1/plugins/") and path.endswith("/brush-state"):
                    self._ok({"reserved_bytes": 0, "used_bytes": 0, "tasks": []})
                elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions"):
                    self._ok({"effective_grants": [], "grants": []})
                elif path.startswith("/api/v1/plugins/"):
                    pid = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        p = self.plugins.get(pid, {"id": pid, "revision": 1, "lifecycle": "enabled"})
                    self._ok(p)
                elif path.startswith("/api/v1/plugin-operations/"):
                    op_id = path.rsplit("/", 1)[-1]
                    self._ok(
                        {
                            "operation_id": op_id,
                            "state": "succeeded",
                            "result": {
                                "preparation_id": f"prep-{op_id}",
                                "consent_token": f"consent-{op_id}",
                                "grants": [],
                            },
                        }
                    )
                elif path == "/api/v1/jobs/stream":
                    sse = b'event: job\ndata: {"id":"j1","state":"succeeded"}\n\n'
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(sse)))
                    self.end_headers()
                    self.wfile.write(sse)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_POST(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/auth/login":
                    tok = f"broken-{uuid.uuid4().hex}"
                    self._ok(
                        {"token": tok, "user": self.admin_user},
                        extra_headers=[("Set-Cookie", f"mc_session={tok}; Path=/; HttpOnly")],
                    )
                elif path == "/api/v1/plugin-reauth":
                    purpose = req.get("purpose")
                    if purpose not in VALID_REAUTH_PURPOSES:
                        self._send_json(400, {"ok": False, "error": {"code": "auth.invalid", "message": f"invalid purpose: {purpose!r}"}})
                        return
                    self._ok({"reauth_token": f"reauth-{uuid.uuid4().hex}", "purpose": purpose})
                elif path == "/api/v1/plugin-trust-keys/prepare":
                    self._ok({"fingerprint": "f" * 64, "consent_token": "c-trust"})
                elif path == "/api/v1/plugin-trust-keys":
                    self._ok({"id": str(uuid.uuid4()), "fingerprint": req.get("fingerprint", "f" * 64)}, status=201)
                elif path == "/api/v1/plugin-uploads":
                    self._ok({"upload_id": str(uuid.uuid4())}, status=201)
                elif path in ("/api/v1/plugin-installations/prepare", "/api/v1/plugin-installations"):
                    self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions/prepare"):
                    self._ok({"consent_token": "c-perm", "grants": req.get("requested_grants", [])})
                elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions/grant"):
                    if "reauth_token" in req:
                        self._send_json(400, {"ok": False, "error": {"code": "plugin.invalid_input", "message": "unexpected reauth_token"}})
                        return
                    self._ok({"granted": True})
                elif path.startswith("/api/v1/plugins/") and path.endswith(("/enable", "/disable", "/uninstall", "/browser-checks", "/commands")):
                    self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
                elif path == "/api/v1/libraries":
                    lid = str(uuid.uuid4())
                    lib = {"id": lid, "name": req.get("name", "lib"), "kind": req.get("kind", "movie"), "root_paths": req.get("root_paths", [])}
                    with self.state_lock:
                        self.libs[lid] = lib
                    self._ok(lib, status=201)
                elif path == "/api/v1/downloaders":
                    self._ok({"id": str(uuid.uuid4()), "name": req.get("name", "qb")}, status=201)
                elif path == "/api/v1/sites":
                    self._ok({"id": str(uuid.uuid4()), "name": req.get("name", "site"), "profile_id": req.get("profile_id", "pterclub")}, status=201)
                elif path == "/api/v1/subscriptions":
                    sid = str(uuid.uuid4())
                    sub = {"id": sid, "library_id": req.get("library_id")}
                    with self.state_lock:
                        self.subs[sid] = sub
                    self._ok(sub, status=201)
                elif path == "/api/v1/jobs/tick":
                    self._ok({"ran": 0, "kinds": [], "skipped_maintenance": []})
                elif path.startswith("/api/v1/jobs/") and path.endswith("/run"):
                    self._ok({"status": "created", "queued": True})
                elif path in ("/api/v1/downloaders/submit",) or (
                    path.startswith("/api/v1/subscriptions/") and path.endswith("/run")
                ) or (
                    path.startswith("/api/v1/sites/") and path.endswith("/check-in")
                ):
                    # Broken no-op: returns 200 without invoking mocks or hooks
                    self._ok({"ok": True, "accepted": True})
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_PUT(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/settings/browser":
                    self._ok({"cdp": req.get("cdp", {"enabled": False})})
                elif path.startswith("/api/v1/libraries/") and path.endswith("/default"):
                    self._ok({"default": True})
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_PATCH(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/settings/transfer":
                    with self.state_lock:
                        self.transfer_rev += 1
                        if "watch_intake" in req:
                            self.watch_intake = req["watch_intake"]
                        self._ok({"revision": self.transfer_rev, "watch_intake": self.watch_intake})
                elif path.startswith("/api/v1/libraries/"):
                    lid = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        if lid in self.libs and "root_paths" in req:
                            self.libs[lid]["root_paths"] = list(req["root_paths"])
                    self._ok({"operation_id": str(uuid.uuid4()), "state": "queued"}, status=202)
                elif path.startswith("/api/v1/sites/"):
                    self._ok({"id": path.rsplit("/", 1)[-1]})
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        full_reg = build_default_registry()
        cases_by_id = {c.id: c for c in full_reg.cases}
        subset_reg = SuiteRegistry()
        for cid in hardened_ids:
            subset_reg.register(cases_by_id[cid])

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), BrokenPhase1Handler)
        thr = threading.Thread(target=srv.serve_forever, daemon=True)
        thr.start()
        try:
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                broken_cli = Path(isolated_tmp_root) / "broken_cli.sh"
                broken_cli.write_text("#!/bin/sh\necho '{}'\nexit 0\n", encoding="utf-8")
                os.chmod(broken_cli, 0o755)
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    url = f"http://127.0.0.1:{srv.server_address[1]}"
                    args = parse_args(["--target-url", url, "--bin", str(broken_cli), "--tier", "1", "--tier", "2"])
                    report = run_suite(args, registry=subset_reg)
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root

            summary = report["summary"]
            suite_ref.assertEqual(summary["total"], len(hardened_ids))
            suite_ref.assertEqual(
                summary["passed"],
                0,
                f"Broken-Phase-1 vacuous passes detected: {[r['id'] for r in report['results'] if r['status'] == 'pass']}",
            )
            suite_ref.assertEqual(summary["unimplemented"], 0, f"Unexpected UNIMPL: {report['results']}")
            suite_ref.assertEqual(summary["failed"], len(hardened_ids))
            for r in report["results"]:
                err_txt = r.get("error") or ""
                for bad_exc in ("NameError", "AttributeError", "TypeError"):
                    suite_ref.assertNotIn(bad_exc, err_txt, f"Case {r['id']} crashed with {bad_exc}: {err_txt}")
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_challenger1_hardening_edge_cases(self) -> None:
        # 1. MockQbitServer strips embedded NUL bytes in savepath
        with tempfile.TemporaryDirectory() as td:
            save_dir = Path(td) / "qb_null"
            with MockQbitServer(require_auth=False, real_save_dir=save_dir) as qb:
                c = ApiClient(qb.url)
                res = c.raw_request(
                    "POST",
                    "/api/v2/torrents/add",
                    body=urllib.parse.urlencode(
                        {"urls": "http://127.0.0.1/null.torrent", "savepath": "/downloads/bad\x00dir"}
                    ).encode("utf-8"),
                    headers={"Content-Type": "application/x-www-form-urlencoded"},
                )
                self.assertEqual(res.status, 200)
                self.assertEqual(res.body, b"Ok.")
                created = list(save_dir.rglob("*.mkv"))
                self.assertEqual(len(created), 1)
                self.assertTrue(str(created[0].resolve()).startswith(str(save_dir.resolve())))

        # 2. MockObscuraCdpServer returns HTTP 400 with Content-Length: 0 on non-ASCII Sec-WebSocket-Key
        with MockObscuraCdpServer() as cdp:
            parts = urllib.parse.urlsplit(cdp.url)
            s = socket.create_connection((parts.hostname, parts.port), timeout=2.0)
            try:
                raw_req = (
                    b"GET /ws/browser HTTP/1.1\r\n"
                    b"Host: 127.0.0.1\r\n"
                    + "Sec-WebSocket-Key: \u2603invalid\r\n".encode("utf-8")
                    + b"\r\n"
                )
                s.sendall(raw_req)
                buf = b""
                while b"\r\n\r\n" not in buf:
                    chunk = s.recv(4096)
                    if not chunk:
                        break
                    buf += chunk
                header_text = buf.decode("latin-1")
                self.assertTrue(header_text.startswith("HTTP/1.1 400 "), f"Expected 400, got {header_text!r}")
                self.assertIn("content-length: 0", header_text.lower())
            finally:
                s.close()

        # 3. run_plugin_cli returns CompletedProcess for bogus plugin subcommand, UNIMPL only for missing 'plugin'
        with tempfile.TemporaryDirectory() as td:
            bogus_sub_bin = Path(td) / "cli_bogus_sub.sh"
            bogus_sub_bin.write_text(
                "#!/bin/sh\necho \"error: unrecognized subcommand 'bogus'\" >&2\nexit 2\n",
                encoding="utf-8",
            )
            os.chmod(bogus_sub_bin, 0o755)

            missing_plugin_bin = Path(td) / "cli_missing_plugin.sh"
            missing_plugin_bin.write_text(
                "#!/bin/sh\necho \"error: unrecognized subcommand 'plugin'\" >&2\nexit 2\n",
                encoding="utf-8",
            )
            os.chmod(missing_plugin_bin, 0o755)

            ctx = TestContext(
                base_url="http://127.0.0.1:1",
                client=ApiClient("http://127.0.0.1:1"),
                admin_password="p",
                cli_token="t",
                bin_path=bogus_sub_bin,
            )
            try:
                proc = run_plugin_cli(ctx, ["bogus"])
                self.assertEqual(proc.returncode, 2)
                self.assertIn("unrecognized subcommand 'bogus'", proc.stderr)

                ctx.bin_path = missing_plugin_bin
                with self.assertRaises(UnimplementedFeatureError):
                    run_plugin_cli(ctx, ["validate", td])
            finally:
                ctx.close()

    def test_obscura_cdp_keepalive_reuse_across_error_responses(self) -> None:
        with MockObscuraCdpServer(required_cdp_token="secret-tok") as cdp:
            parts = urllib.parse.urlsplit(cdp.url)
            conn = http.client.HTTPConnection(parts.hostname, parts.port, timeout=3.0)
            try:
                for method, path, expected_status in (
                    ("GET", "/json/version", 200),
                    ("GET", "/nonexistent-path", 404),
                    ("GET", "/ws/browser?token=wrong", 401),
                    ("GET", "/ws/browser?token=secret-tok", 400),
                    ("GET", "/json/version", 200),
                ):
                    conn.request(method, path)
                    resp = conn.getresponse()
                    self.assertEqual(
                        resp.status,
                        expected_status,
                        f"Unexpected status for {path}: {resp.status}",
                    )
                    body = resp.read()
                    if expected_status == 200:
                        data = json.loads(body.decode("utf-8"))
                        self.assertEqual(data["webSocketDebuggerUrl"], cdp.ws_url)
                    else:
                        self.assertEqual(body, b"")
            finally:
                conn.close()

    def test_trigger_job_queue_contention_and_max_ticks_enforcement(self) -> None:
        suite_ref = self

        class ContendedJobsHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            state_lock = threading.Lock()
            scenario = "maintenance_then_transfer"
            tick_calls = 0
            run_calls = []
            jobs_queries = []

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _ok(self, data: object) -> None:
                body = json.dumps({"ok": True, "data": data}).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                if parsed.path == "/api/v1/jobs":
                    with ContendedJobsHandler.state_lock:
                        ContendedJobsHandler.jobs_queries.append(parsed.query)
                        # Verify scope=system is NOT used so subscribe_search is visible
                        suite_ref.assertNotIn("scope=system", parsed.query)
                        tc = ContendedJobsHandler.tick_calls
                        sc = ContendedJobsHandler.scenario
                        if sc == "busy_running":
                            status = "running" if tc < 2 else "succeeded"
                        elif sc == "busy_running_clears_while_other_ran":
                            status = "running" if tc == 0 else "succeeded"
                        else:
                            status = "queued"

                        if sc == "scope_key_none_to_ts_contention":
                            self._ok(
                                [
                                    {
                                        "id": "def-sub-other",
                                        "kind": "subscribe_search",
                                        "scope_key": "subscribe:sub-other",
                                        "enabled": True,
                                        "last_status": "queued",
                                        "last_finished_at": None if tc < 1 else 1001,
                                    },
                                    {
                                        "id": "def-sub-new",
                                        "kind": "subscribe_search",
                                        "scope_key": "subscribe:sub-new",
                                        "enabled": True,
                                        "last_status": "queued",
                                        "last_finished_at": None if tc < 2 else 1002,
                                    },
                                ]
                            )
                        elif sc == "scope_key_reconciled_after_first_tick":
                            if tc == 0:
                                self._ok([])
                            else:
                                self._ok(
                                    [
                                        {
                                            "id": "def-sub-late",
                                            "kind": "subscribe_search",
                                            "scope_key": "subscribe:sub-late",
                                            "enabled": True,
                                            "last_status": "queued",
                                            "last_finished_at": None if tc < 2 else 2002,
                                        }
                                    ]
                                )
                        else:
                            self._ok(
                                [
                                    {
                                        "id": "def-transfer",
                                        "kind": "transfer",
                                        "scope_key": "global",
                                        "enabled": True,
                                        "last_status": status,
                                        "last_finished_at": 100 if tc < 2 else 200,
                                    },
                                    {
                                        "id": "def-sub-search",
                                        "kind": "subscribe_search",
                                        "scope_key": "subscribe:sub-123",
                                        "enabled": True,
                                        "last_status": "queued",
                                        "last_finished_at": 100 if tc < 11 else 300,
                                    },
                                ]
                            )
                else:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()

            def do_POST(self) -> None:
                self._read()
                path = urllib.parse.urlsplit(self.path).path
                if path.startswith("/api/v1/jobs/") and path.endswith("/run"):
                    def_id = path.split("/")[-2]
                    with ContendedJobsHandler.state_lock:
                        ContendedJobsHandler.run_calls.append(def_id)
                    self._ok({"status": "created", "queued": True})
                elif path == "/api/v1/jobs/tick":
                    with ContendedJobsHandler.state_lock:
                        ContendedJobsHandler.tick_calls += 1
                        tc = ContendedJobsHandler.tick_calls
                        sc = ContendedJobsHandler.scenario
                    if sc == "maintenance_then_transfer":
                        if tc < 3:
                            self._ok({"ran": 0, "kinds": [], "skipped_maintenance": ["database_maintenance"]})
                        else:
                            self._ok({"ran": 1, "kinds": ["transfer"], "skipped_maintenance": []})
                    elif sc == "busy_running":
                        if tc < 3:
                            self._ok({"ran": 0, "kinds": [], "skipped_maintenance": []})
                        else:
                            self._ok({"ran": 1, "kinds": ["transfer"], "skipped_maintenance": []})
                    elif sc == "busy_running_clears_while_other_ran":
                        if tc < 2:
                            self._ok({"ran": 1, "kinds": ["watch_intake"], "skipped_maintenance": []})
                        else:
                            self._ok({"ran": 1, "kinds": ["transfer"], "skipped_maintenance": []})
                    elif sc in ("scope_key_none_to_ts_contention", "scope_key_reconciled_after_first_tick"):
                        self._ok({"ran": 1, "kinds": ["subscribe_search"], "skipped_maintenance": []})
                    elif sc == "long_queue_subscribe_search":
                        if tc <= 10:
                            self._ok({"ran": 2, "kinds": ["watch_intake", "transfer"], "skipped_maintenance": []})
                        else:
                            self._ok({"ran": 1, "kinds": ["subscribe_search"], "skipped_maintenance": []})
                    elif sc == "always_ran_zero":
                        self._ok({"ran": 0, "kinds": [], "skipped_maintenance": []})
                    elif sc == "always_other_kinds":
                        self._ok({"ran": 2, "kinds": ["watch_intake", "check_in"], "skipped_maintenance": []})
                else:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), ContendedJobsHandler)
        thr = threading.Thread(target=srv.serve_forever, daemon=True)
        thr.start()
        try:
            url = f"http://127.0.0.1:{srv.server_address[1]}"
            ctx = TestContext(
                base_url=url,
                client=ApiClient(url, token="tok"),
                admin_password="p",
                cli_token="t",
            )
            try:
                # 1. Maintenance skip (ran=0) must NOT return early; succeeds on tick 3
                ContendedJobsHandler.scenario = "maintenance_then_transfer"
                ContendedJobsHandler.tick_calls = 0
                ContendedJobsHandler.run_calls = []
                res1 = trigger_job(ctx, "transfer", max_ticks=6)
                self.assertIn("transfer", res1["kinds"])
                self.assertEqual(ContendedJobsHandler.tick_calls, 3)

                # 2. Busy concurrency key (last_status=running, ran=0) waits & avoids spamming /run while running
                ContendedJobsHandler.scenario = "busy_running"
                ContendedJobsHandler.tick_calls = 0
                ContendedJobsHandler.run_calls = []
                res2 = trigger_job(ctx, "transfer", max_ticks=6)
                self.assertIn("transfer", res2["kinds"])
                self.assertEqual(ContendedJobsHandler.tick_calls, 3)
                # Initial expedite + 1 re-expedite after busy cleared on tick 2 (skipped while running on tick 1)
                self.assertEqual(ContendedJobsHandler.run_calls, ["def-transfer", "def-transfer"])

                # 3. subscribe_search survives >8 ticks of other jobs via default max_ticks=16
                ContendedJobsHandler.scenario = "long_queue_subscribe_search"
                ContendedJobsHandler.tick_calls = 0
                ContendedJobsHandler.run_calls = []
                res3 = trigger_job(ctx, "subscribe_search", scope_key="subscribe:sub-123")
                self.assertIn("subscribe_search", res3["kinds"])
                self.assertEqual(ContendedJobsHandler.tick_calls, 11)
                self.assertGreaterEqual(len(ContendedJobsHandler.run_calls), 1)
                self.assertTrue(all(x == "def-sub-search" for x in ContendedJobsHandler.run_calls))

                # 4. max_ticks exhaustion raises AssertionError when ran=0 persists
                ContendedJobsHandler.scenario = "always_ran_zero"
                ContendedJobsHandler.tick_calls = 0
                with self.assertRaises(AssertionError) as cm_zero:
                    trigger_job(ctx, "transfer", max_ticks=4)
                self.assertIn("did not execute within 4 ticks", str(cm_zero.exception))
                self.assertEqual(ContendedJobsHandler.tick_calls, 4)

                # 5. max_ticks exhaustion raises AssertionError when only other kinds run
                ContendedJobsHandler.scenario = "always_other_kinds"
                ContendedJobsHandler.tick_calls = 0
                with self.assertRaises(AssertionError) as cm_other:
                    trigger_job(ctx, "transfer", max_ticks=4)
                self.assertIn("did not execute within 4 ticks", str(cm_other.exception))
                self.assertEqual(ContendedJobsHandler.tick_calls, 4)

                # 6. brand-new scope_key (last_finished_at=None) must NOT return early when another subscribe_search finishes
                ContendedJobsHandler.scenario = "scope_key_none_to_ts_contention"
                ContendedJobsHandler.tick_calls = 0
                ContendedJobsHandler.run_calls = []
                res6 = trigger_job(ctx, "subscribe_search", scope_key="subscribe:sub-new", max_ticks=6)
                self.assertIn("subscribe_search", res6["kinds"])
                self.assertEqual(ContendedJobsHandler.tick_calls, 2)
                self.assertIn("def-sub-new", ContendedJobsHandler.run_calls)

                # 7. busy running clears while other jobs run (ran >= 1) still re-expedites target
                ContendedJobsHandler.scenario = "busy_running_clears_while_other_ran"
                ContendedJobsHandler.tick_calls = 0
                ContendedJobsHandler.run_calls = []
                res7 = trigger_job(ctx, "transfer", max_ticks=6)
                self.assertIn("transfer", res7["kinds"])
                self.assertEqual(ContendedJobsHandler.tick_calls, 2)
                self.assertEqual(ContendedJobsHandler.run_calls, ["def-transfer", "def-transfer"])

                # 8. scope_key reconciled after tick 1 is discovered, expedited, and awaited until finished
                ContendedJobsHandler.scenario = "scope_key_reconciled_after_first_tick"
                ContendedJobsHandler.tick_calls = 0
                ContendedJobsHandler.run_calls = []
                res8 = trigger_job(ctx, "subscribe_search", scope_key="subscribe:sub-late", max_ticks=6)
                self.assertIn("subscribe_search", res8["kinds"])
                self.assertEqual(ContendedJobsHandler.tick_calls, 2)
                self.assertIn("def-sub-late", ContendedJobsHandler.run_calls)
            finally:
                ctx.close()
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_full_broken_phase1_and_smart_boost_sweep_zero_vacuous_passes(self) -> None:
        suite_ref = self

        # Part A: Full 264-case sweep against FullBrokenPhase1Handler (0 vacuous passes)
        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), _make_sweep_broken_handler(smart=False))
        thr = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thr.start()
        try:
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                broken_cli = Path(isolated_tmp_root) / "broken_cli.sh"
                broken_cli.write_text("#!/bin/sh\necho '{}'\nexit 0\n", encoding="utf-8")
                os.chmod(broken_cli, 0o755)
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    url = f"http://127.0.0.1:{srv.server_address[1]}"
                    args = parse_args(["--target-url", url, "--bin", str(broken_cli), "--tier", "1", "--tier", "2"])
                    report = run_suite(args, registry=build_default_registry())
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root

            summary = report["summary"]
            suite_ref.assertGreaterEqual(summary["total"], 264)
            suite_ref.assertEqual(
                summary["passed"],
                0,
                f"FullBrokenPhase1Handler vacuous passes: {[r['id'] for r in report['results'] if r['status'] == 'pass']}",
            )
            suite_ref.assertLessEqual(summary["unimplemented"], 5)
            for r in report["results"]:
                err_txt = r.get("error") or ""
                for bad_exc in ("NameError", "AttributeError", "TypeError"):
                    suite_ref.assertNotIn(bad_exc, err_txt, f"Case {r['id']} crashed with {bad_exc}: {err_txt}")
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

        # Part B: SmartBrokenPhase1Handler across ALL 264 Tier 1 + Tier 2 cases with both exit 0 and exit 1 CLI stubs.
        # Verifies 0 crashes (including KeyError on T1-F26-04) and 0 vacuous passes on all hardened/broken targets.
        must_fail_ids = {
            "T1-F05-05",
            "T1-F08-01",
            "T1-F08-05",
            "T1-F16-04",
            "T1-F16-05",
            "T1-F14-02",
            "T1-F18-01",
            "T1-F19-01",
            "T1-F19-04",
            "T1-F20-01",
            "T1-F21-04",
            "T1-F21-05",
            "T1-F06-05",
            "T1-F24-03",
            "T1-F24-04",
            "T1-F03-03",
            "T1-F03-05",
            "T1-F22-04",
            "T1-F02-03",
            "T1-F25-01",
            "T1-F26-02",
            "T1-F26-04",
            "T2-F12-02",
            "T2-F14-02",
            "T2-F14-04",
            "T2-F16-05",
            "T2-F19-03",
            "T2-F19-04",
            "T2-F23-01",
            "T2-F23-04",
            "T2-F26-01",
        }
        # Only cases whose exact positive contract is genuinely implemented by SmartBrokenPhase1Handler
        expected_smart_impl_passes = {
            "T1-F01-SMOKE-02",
            "T1-F01-01",
            "T1-F01-02",
            "T1-F01-03",
            "T1-F01-04",
            "T1-F01-05",
            "T1-F02-01",
            "T1-F02-02",
            "T1-F03-01",
            "T1-F05-01",
            "T1-F05-02",
            "T1-F06-01",
            "T1-F06-02",
            "T1-F07-04",
            "T1-F08-03",
            "T1-F11-04",
            "T1-F22-01",
            "T1-F22-02",
            "T1-F22-05",
            "T1-F24-01",
            "T1-F25-02",
            "T1-F25-04",
        }

        for cli_label, cli_body in (
            ("cli0", "#!/bin/sh\necho '{}'\nexit 0\n"),
            ("cli1", "#!/bin/sh\necho 'invalid plugin' >&2\nexit 1\n"),
        ):
            srv2 = _ThreadedStubHTTPServer(("127.0.0.1", 0), _make_sweep_broken_handler(smart=True))
            thr2 = threading.Thread(target=srv2.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
            thr2.start()
            try:
                with tempfile.TemporaryDirectory() as isolated_tmp_root:
                    broken_cli = Path(isolated_tmp_root) / "broken_cli.sh"
                    broken_cli.write_text(cli_body, encoding="utf-8")
                    os.chmod(broken_cli, 0o755)
                    prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                    os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                    try:
                        url = f"http://127.0.0.1:{srv2.server_address[1]}"
                        args = parse_args(["--target-url", url, "--bin", str(broken_cli), "--tier", "1", "--tier", "2"])
                        report2 = run_suite(args, registry=build_default_registry())
                    finally:
                        if prev_root is None:
                            os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                        else:
                            os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root

                summary2 = report2["summary"]
                suite_ref.assertGreaterEqual(summary2["total"], 264)
                suite_ref.assertLessEqual(summary2["unimplemented"], 5)
                passed_ids = {r["id"] for r in report2["results"] if r["status"] == "pass"}
                bad_must_fail = sorted(passed_ids & must_fail_ids)
                suite_ref.assertEqual(
                    bad_must_fail,
                    [],
                    f"SmartBrokenPhase1Handler ({cli_label}) vacuous passes on must-fail cases: {bad_must_fail}",
                )
                unexpected_passes = sorted(passed_ids - expected_smart_impl_passes)
                suite_ref.assertEqual(
                    unexpected_passes,
                    [],
                    f"SmartBrokenPhase1Handler ({cli_label}) unexpected passes: {unexpected_passes}",
                )
                for r in report2["results"]:
                    err_txt = r.get("error") or ""
                    for bad_exc in ("NameError", "KeyError", "AttributeError", "TypeError"):
                        suite_ref.assertNotIn(
                            bad_exc,
                            err_txt,
                            f"Case {r['id']} ({cli_label}) crashed with {bad_exc}: {err_txt}",
                        )
            finally:
                srv2.shutdown()
                srv2.server_close()
                thr2.join(timeout=2.0)

    def test_developer_mode_reauth_purpose_and_permission_grant_contracts(self) -> None:
        with SelfTestStubServer() as stub:
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    cli = ApiClient(stub.url)
                    cli.login("admin", stub.admin_password)
                    ctx = TestContext(
                        base_url=stub.url,
                        client=cli,
                        admin_password=stub.admin_password,
                        cli_token=stub.cli_token,
                    )
                    with ctx:
                        # 1. set_developer_mode uses developer_mode (without "enabled") and sends no reauth_token on disable
                        s0 = set_developer_mode(ctx, False)
                        self.assertIs(s0["developer_mode"], False)
                        self.assertNotIn("enabled", s0)
                        s1 = set_developer_mode(ctx, True)
                        self.assertIs(s1["developer_mode"], True)
                        self.assertNotIn("enabled", s1)
                        s2 = set_developer_mode(ctx, False)
                        self.assertIs(s2["developer_mode"], False)
                        self.assertNotIn("enabled", s2)

                        # 2. Legacy hyphenated P1c ReauthPurpose strings are rejected with 400
                        for bad_purpose in (
                            "plugin-upload",
                            "trust-key-pin",
                            "trust-key-revoke",
                            "developer-mode-enable",
                            "developer-mode-disable",
                            "plugin-upgrade-reset",
                            "plugin-state-reset",
                            "plugin-permission-grant",
                        ):
                            assert_api_error(
                                lambda p=bad_purpose: ctx.call_phase1(
                                    "POST",
                                    "/api/v1/plugin-reauth",
                                    json_body={"password": stub.admin_password, "purpose": p},
                                ),
                                (400,),
                                label=f"legacy purpose {bad_purpose}",
                            )

                        # 3. All 7 M1 colon-separated ReauthPurpose variants succeed
                        for good_purpose in (
                            "trust-key:create",
                            "trust-key:revoke",
                            "developer-mode:enable",
                            "plugin:upgrade",
                            "plugin:rollback",
                            "plugin:state-reset",
                            "plugin:upload",
                        ):
                            tok = obtain_reauth(ctx, purpose=good_purpose)
                            self.assertTrue(tok)
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root

    def test_t1_f26_02_rejects_bad_asset_content_type_or_missing_nosniff(self) -> None:
        class BadAssetHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            asset_mode = "bad_ct"

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _ok(self, data: object, status: int = 200, extra_headers: list = None) -> None:
                body = json.dumps({"ok": True, "data": data}).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                if path == "/api/v1/plugins":
                    self._ok([{"id": "builtin.checkin", "revision": 1, "lifecycle": "enabled", "package_hash": "a" * 64}])
                elif path == "/api/v1/plugins/builtin.checkin/ui":
                    self._ok(
                        {
                            "plugin_id": "builtin.checkin",
                            "package_hash": "a" * 64,
                            "entry_url": "/api/v1/plugin-ui-assets/builtin.checkin/" + ("a" * 64) + "/ui/assets/remoteEntry.js",
                            "surfaces": [
                                {"id": "cfg", "kind": "config"},
                                {"id": "pg", "kind": "page"},
                                {"id": "app", "kind": "app"},
                                {"id": "dash", "kind": "dashboard"},
                            ],
                        }
                    )
                elif path.startswith("/api/v1/plugin-ui-assets/"):
                    if self.asset_mode == "empty":
                        body = b""
                        ct = "application/javascript"
                        headers = [("X-Content-Type-Options", "nosniff")]
                    elif self.asset_mode == "no_nosniff":
                        body = b"export const ok = true;\n"
                        ct = "application/javascript"
                        headers = []
                    else:
                        body = b"<html>not js</html>"
                        ct = "text/html"
                        headers = [("X-Content-Type-Options", "nosniff")]
                    self.send_response(200)
                    self.send_header("Content-Type", ct)
                    self.send_header("Content-Length", str(len(body)))
                    for k, v in headers:
                        self.send_header(k, v)
                    self.end_headers()
                    self.wfile.write(body)
                else:
                    self._ok({})

            def do_POST(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                self._read()
                if path == "/api/v1/auth/login":
                    tok = f"tok-{uuid.uuid4().hex}"
                    self._ok(
                        {"token": tok, "user": {"id": "u1", "login": "admin", "role": "admin"}},
                        extra_headers=[("Set-Cookie", f"mc_session={tok}; Path=/; HttpOnly")],
                    )
                elif path == "/api/v1/plugin-reauth":
                    self._ok({"reauth_token": "r1", "purpose": "plugin-ui-trust"})
                elif path.endswith("/ui-trust/prepare"):
                    self._ok({"consent_token": "c1", "trust_revision": 0})
                elif path.endswith("/ui-trust/grant"):
                    self._ok({"status": "trusted", "revision": 1})
                else:
                    self._ok({})

        full_reg = build_default_registry()
        f26_02 = [c for c in full_reg.cases if c.id == "T1-F26-02"][0]
        sub_reg = SuiteRegistry()
        sub_reg.register(f26_02)
        for mode in ("bad_ct", "no_nosniff", "empty"):
            BadAssetHandler.asset_mode = mode
            srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), BadAssetHandler)
            thr = threading.Thread(target=srv.serve_forever, daemon=True)
            thr.start()
            try:
                with tempfile.TemporaryDirectory() as isolated_tmp_root:
                    prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                    os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                    try:
                        url = f"http://127.0.0.1:{srv.server_address[1]}"
                        args = parse_args(["--target-url", url, "--tier", "1"])
                        rep = run_suite(args, registry=sub_reg)
                    finally:
                        if prev_root is None:
                            os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                        else:
                            os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root
                self.assertEqual(rep["summary"]["passed"], 0, f"T1-F26-02 vacuously passed on {mode}: {rep}")
            finally:
                srv.shutdown()
                srv.server_close()
                thr.join(timeout=2.0)

    def test_secret_config_schema_and_config_object_grant_contract(self) -> None:
        sec_schema = build_secret_config_schema()
        self.assertIn("profile_label", sec_schema["properties"])
        self.assertNotIn("site_id", sec_schema["properties"])
        self.assertIn("profile_label", sec_schema["required"])

        def _extract_upload(content_type: str, body: bytes) -> tuple:
            if "boundary=" in content_type:
                boundary = content_type.split("boundary=", 1)[1].strip().strip('"').encode("latin-1")
                for part in body.split(b"--" + boundary):
                    if b'name="file"' in part and b"\r\n\r\n" in part:
                        payload = part.split(b"\r\n\r\n", 1)[1]
                        if payload.endswith(b"\r\n"):
                            payload = payload[:-2]
                        sha = hashlib.sha256(payload).hexdigest()
                        pid = ""
                        with zipfile.ZipFile(io.BytesIO(payload), "r") as zf:
                            for line in zf.read("plugin.toml").decode("utf-8", errors="ignore").splitlines():
                                if line.strip().startswith("id ="):
                                    pid = line.split("=", 1)[1].strip().strip('"').strip("'")
                                    break
                        return sha, pid
            return hashlib.sha256(body).hexdigest(), ""

        class ConfigGrantContractHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            state_lock = threading.Lock()
            uploads = {}
            preps = {}
            ops = {}
            plugins = {}
            configs = {}
            sites = {}

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _send_json(self, status: int, payload: object, extra_headers: list = None) -> None:
                body = json.dumps(payload).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def _ok(self, data: object, status: int = 200, extra_headers: list = None) -> None:
                self._send_json(status, {"ok": True, "data": data}, extra_headers=extra_headers)

            def do_GET(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                if path == "/api/v1/health":
                    self._ok({"status": "ok", "read_only": False})
                elif path == "/api/v1/auth/me":
                    self._ok({"id": "u1", "login": "admin", "role": "admin", "enabled": True})
                elif path.startswith("/api/v1/plugin-operations/"):
                    op_id = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        op = self.ops.get(op_id, {"operation_id": op_id, "state": "succeeded"})
                    self._ok(op)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/config"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        cfg = self.configs.get(pid) or {
                            "plugin_id": pid,
                            "revision": 1,
                            "config_revision": 1,
                            "schema_revision": "1",
                            "values": {},
                            "secrets": {"api_token": {"configured": False}},
                        }
                    self._ok(cfg)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/ui"):
                    pid = path.split("/")[4]
                    self._ok({"plugin_id": pid, "surfaces": [{"id": "cfg", "kind": "config", "editable": True}]})
                elif path.startswith("/api/v1/plugins/") and path.endswith("/audit"):
                    pid = path.split("/")[4]
                    self._ok([{"operation_id": str(uuid.uuid4()), "plugin_id": pid, "action": "install", "state": "succeeded"}])
                elif path.startswith("/api/v1/plugins/"):
                    pid = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        p = self.plugins.get(pid)
                    if p is None:
                        self._send_json(404, {"ok": False, "error": {"code": "plugin.missing", "message": "not found"}})
                    else:
                        self._ok(p)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_POST(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/auth/login":
                    tok = f"tok-{uuid.uuid4().hex}"
                    self._ok(
                        {"token": tok, "user": {"id": "u1", "login": "admin", "role": "admin", "enabled": True}},
                        extra_headers=[("Set-Cookie", f"mc_session={tok}; Path=/; HttpOnly")],
                    )
                elif path == "/api/v1/plugin-reauth":
                    self._ok({"reauth_token": f"r-{uuid.uuid4().hex}", "purpose": req.get("purpose"), "expires_at": "2099-01-01T00:00:00Z"})
                elif path == "/api/v1/plugin-trust-keys/prepare":
                    self._ok({"key_id": "0" * 16, "fingerprint": "f" * 64, "consent_token": "c-tk", "expires_at": "2099-01-01T00:00:00Z"})
                elif path == "/api/v1/plugin-trust-keys":
                    self._ok({"id": str(uuid.uuid4()), "key_id": "0" * 16, "status": "trusted", "revision": 1}, status=201)
                elif path == "/api/v1/plugin-uploads":
                    uid = str(uuid.uuid4())
                    sha, pid = _extract_upload(self.headers.get("Content-Type", ""), raw)
                    with self.state_lock:
                        self.uploads[uid] = (sha, pid)
                    self._ok({"upload_id": uid, "hash": sha, "signature_status": "verified"}, status=201)
                elif path == "/api/v1/plugin-installations/prepare":
                    op_id = str(uuid.uuid4())
                    prep_id = f"prep-{op_id}"
                    sha, pid = self.uploads[req["upload_id"]]
                    with self.state_lock:
                        self.preps[prep_id] = (sha, pid)
                        self.ops[op_id] = {
                            "operation_id": op_id,
                            "state": "succeeded",
                            "result": {
                                "preparation_id": prep_id,
                                "consent_token": f"c-{op_id}",
                                "package_hash": sha,
                                "grant_diff": {"added": [{"capability": "kv", "scope": "Global", "sensitive": False}], "kept": [], "removed": [], "required": ["kv"]},
                            },
                        }
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/plugin-installations":
                    op_id = str(uuid.uuid4())
                    sha, pid = self.preps[req["preparation_id"]]
                    with self.state_lock:
                        self.plugins[pid] = {
                            "id": pid,
                            "revision": 1,
                            "config_revision": 1,
                            "generation": 0,
                            "lifecycle": "installed-disabled",
                            "enabled": False,
                            "package_hash": sha,
                        }
                        self.configs[pid] = {
                            "plugin_id": pid,
                            "revision": 1,
                            "config_revision": 1,
                            "schema_revision": "1",
                            "values": {},
                            "secrets": {"api_token": {"configured": False}},
                        }
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_PUT(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path.startswith("/api/v1/plugins/") and path.endswith("/config"):
                    pid = path.split("/")[4]
                    vals = req.get("values") or {}
                    secrets_in = req.get("secrets") or {}
                    # Enforce crates/api/src/plugins/config_ops.rs:43-59 validate_config_object_grants
                    if isinstance(vals, dict) and isinstance(vals.get("site_id"), str):
                        try:
                            sid_norm = str(uuid.UUID(vals["site_id"]))
                        except ValueError:
                            self._send_json(400, {"ok": False, "error": {"code": "plugin.scope_invalid", "message": "invalid site_id UUID"}})
                            return
                        if sid_norm not in self.sites:
                            self._send_json(400, {"ok": False, "error": {"code": "plugin.scope_invalid", "message": "referenced site_id does not exist"}})
                            return
                    for k, act in secrets_in.items():
                        if k != "api_token":
                            self._send_json(400, {"ok": False, "error": {"code": "plugin.config_invalid", "message": f"unknown secret key {k}"}})
                            return
                        if isinstance(act, dict) and act.get("action") == "replace" and not act.get("value"):
                            self._send_json(400, {"ok": False, "error": {"code": "plugin.config_invalid", "message": "empty secret value"}})
                            return
                    with self.state_lock:
                        cur = self.configs[pid]
                        sec_cfg = bool(cur["secrets"]["api_token"]["configured"])
                        tok_act = secrets_in.get("api_token", {}).get("action")
                        if tok_act == "replace":
                            sec_cfg = True
                        elif tok_act == "clear":
                            sec_cfg = False
                        new_cfg = {
                            "plugin_id": pid,
                            "revision": int(cur["revision"]) + 1,
                            "config_revision": int(cur["config_revision"]) + 1,
                            "schema_revision": cur["schema_revision"],
                            "values": vals,
                            "secrets": {"api_token": {"configured": sec_cfg}},
                        }
                        self.configs[pid] = new_cfg
                        if pid in self.plugins:
                            self.plugins[pid]["revision"] = new_cfg["revision"]
                            self.plugins[pid]["config_revision"] = new_cfg["config_revision"]
                    self._ok(new_cfg)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        full_reg = build_default_registry()
        target_ids = ("T1-F03-04", "T1-F08-03", "T1-F08-05", "T1-F25-04", "T2-F08-04")
        sub_reg = SuiteRegistry()
        for c in full_reg.cases:
            if c.id in target_ids:
                sub_reg.register(c)

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), ConfigGrantContractHandler)
        thr = threading.Thread(target=srv.serve_forever, daemon=True)
        thr.start()
        try:
            url = f"http://127.0.0.1:{srv.server_address[1]}"
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    args = parse_args(["--target-url", url, "--tier", "1", "--tier", "2"])
                    rep = run_suite(args, registry=sub_reg)
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root
            self.assertEqual(rep["summary"]["total"], 5, rep)
            self.assertEqual(rep["summary"]["passed"], 5, rep)
            self.assertEqual(rep["summary"]["failed"], 0, rep)

            # Negative check: non-UUID site_id in PUT /config is rejected with 400 plugin.scope_invalid
            cli = ApiClient(url)
            cli.login("admin", "e2e-admin-password-secret")
            any_pid = next(iter(ConfigGrantContractHandler.configs))
            cur_cfg = ConfigGrantContractHandler.configs[any_pid]
            err = assert_api_error(
                lambda: cli.put(
                    f"/api/v1/plugins/{any_pid}/config",
                    json_body={"schema_revision": "1", "values": {"site_id": "non-uuid-demo"}, "secrets": {}},
                    if_match=str(cur_cfg["revision"]),
                    idempotency_key="bad-site-id",
                ),
                (400,),
                label="non-UUID site_id rejected",
            )
            self.assertEqual(err.code, "plugin.scope_invalid")
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_t1_f06_03_grants_digest_and_extract_prep_grants_kept(self) -> None:
        # extract_prep_grants must combine added + kept (for both permissions/prepare and upgrades/prepare)
        self.assertEqual(
            extract_prep_grants({"grant_diff": {"added": [], "kept": [{"capability": "kv"}]}}),
            [{"capability": "kv"}],
        )
        self.assertEqual(
            extract_prep_grants(
                {"grant_diff": {"added": [{"capability": "kv"}], "kept": [{"capability": "hooks:search.after"}]}}
            ),
            [{"capability": "kv"}, {"capability": "hooks:search.after"}],
        )

        def _digest(grants: list) -> str:
            canon = sorted(grants, key=lambda g: json.dumps(g, sort_keys=True))
            return hashlib.sha256(json.dumps(canon, sort_keys=True).encode("utf-8")).hexdigest()

        class PermissionsDigestContractHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            state_lock = threading.Lock()
            uploads = {}
            preps = {}
            perm_consents = {}
            ops = {}
            plugins = {}
            grants_by_pid = {}

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _send_json(self, status: int, payload: object, extra_headers: list = None) -> None:
                body = json.dumps(payload).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def _ok(self, data: object, status: int = 200, extra_headers: list = None) -> None:
                self._send_json(status, {"ok": True, "data": data}, extra_headers=extra_headers)

            def do_GET(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                if path == "/api/v1/health":
                    self._ok({"status": "ok", "read_only": False})
                elif path == "/api/v1/auth/me":
                    self._ok({"id": "u1", "login": "admin", "role": "admin", "enabled": True})
                elif path.startswith("/api/v1/plugin-operations/"):
                    op_id = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        op = self.ops.get(op_id, {"operation_id": op_id, "state": "succeeded"})
                    self._ok(op)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        cur_grants = list(self.grants_by_pid.get(pid, []))
                    self._ok({"plugin_id": pid, "effective_grants": cur_grants, "grants": cur_grants})
                elif path.startswith("/api/v1/plugins/"):
                    pid = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        p = self.plugins.get(pid)
                    if p is None:
                        self._send_json(404, {"ok": False, "error": {"code": "plugin.missing", "message": "not found"}})
                    else:
                        self._ok(p)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_POST(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/auth/login":
                    tok = f"tok-{uuid.uuid4().hex}"
                    self._ok(
                        {"token": tok, "user": {"id": "u1", "login": "admin", "role": "admin", "enabled": True}},
                        extra_headers=[("Set-Cookie", f"mc_session={tok}; Path=/; HttpOnly")],
                    )
                elif path == "/api/v1/plugin-reauth":
                    self._ok({"reauth_token": f"r-{uuid.uuid4().hex}", "purpose": req.get("purpose"), "expires_at": "2099-01-01T00:00:00Z"})
                elif path == "/api/v1/plugin-trust-keys/prepare":
                    self._ok({"key_id": "0" * 16, "fingerprint": "f" * 64, "consent_token": "c-tk", "expires_at": "2099-01-01T00:00:00Z"})
                elif path == "/api/v1/plugin-trust-keys":
                    self._ok({"id": str(uuid.uuid4()), "key_id": "0" * 16, "status": "trusted", "revision": 1}, status=201)
                elif path == "/api/v1/plugin-uploads":
                    uid = str(uuid.uuid4())
                    pid = ""
                    if b'name="file"' in raw and b"\r\n\r\n" in raw:
                        boundary = self.headers.get("Content-Type", "").split("boundary=", 1)[1].strip().strip('"').encode("latin-1")
                        for part in raw.split(b"--" + boundary):
                            if b'name="file"' in part and b"\r\n\r\n" in part:
                                payload = part.split(b"\r\n\r\n", 1)[1]
                                if payload.endswith(b"\r\n"):
                                    payload = payload[:-2]
                                with zipfile.ZipFile(io.BytesIO(payload), "r") as zf:
                                    for line in zf.read("plugin.toml").decode("utf-8", errors="ignore").splitlines():
                                        if line.strip().startswith("id ="):
                                            pid = line.split("=", 1)[1].strip().strip('"').strip("'")
                                            break
                    with self.state_lock:
                        self.uploads[uid] = pid
                    self._ok({"upload_id": uid, "hash": "a" * 64, "signature_status": "verified"}, status=201)
                elif path == "/api/v1/plugin-installations/prepare":
                    op_id = str(uuid.uuid4())
                    prep_id = f"prep-{op_id}"
                    pid = self.uploads[req["upload_id"]]
                    suggested = [
                        {"capability": "hooks:search.after", "scope": "Global", "sensitive": False},
                        {"capability": "kv", "scope": "Global", "sensitive": False},
                    ]
                    with self.state_lock:
                        self.preps[prep_id] = pid
                        self.ops[op_id] = {
                            "operation_id": op_id,
                            "state": "succeeded",
                            "result": {
                                "preparation_id": prep_id,
                                "consent_token": f"c-{op_id}",
                                "package_hash": "a" * 64,
                                "grant_diff": {"added": suggested, "kept": [], "removed": [], "required": ["hooks:search.after", "kv"]},
                            },
                        }
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/plugin-installations":
                    op_id = str(uuid.uuid4())
                    pid = self.preps[req["preparation_id"]]
                    with self.state_lock:
                        self.grants_by_pid[pid] = list(req.get("grants", []))
                        self.plugins[pid] = {
                            "id": pid,
                            "revision": 1,
                            "generation": 0,
                            "lifecycle": "installed-disabled",
                            "enabled": False,
                            "package_hash": "a" * 64,
                        }
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/enable"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["lifecycle"] = "enabled"
                        p["enabled"] = True
                        p["generation"] = int(p["generation"]) + 1
                        p["revision"] = int(p["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/disable"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["lifecycle"] = "disabled"
                        p["enabled"] = False
                        p["generation"] = int(p["generation"]) + 1
                        p["revision"] = int(p["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions/prepare"):
                    pid = path.split("/")[4]
                    requested = list(req.get("requested_grants", []))
                    with self.state_lock:
                        cur = list(self.grants_by_pid.get(pid, []))
                        added = [g for g in requested if g not in cur]
                        kept = [g for g in requested if g in cur]
                        removed = [g for g in cur if g not in requested]
                        ctok = f"c-perm-{uuid.uuid4().hex}"
                        self.perm_consents[ctok] = (pid, _digest(requested))
                    self._ok(
                        {
                            "preparation_id": str(uuid.uuid4()),
                            "plugin_id": pid,
                            "consent_token": ctok,
                            "grant_diff": {
                                "added": added,
                                "kept": kept,
                                "removed": removed,
                                "required": ["hooks:search.after", "kv"],
                            },
                        }
                    )
                elif path.startswith("/api/v1/plugins/") and path.endswith("/permissions/grant"):
                    pid = path.split("/")[4]
                    ctok = req.get("consent_token", "")
                    submitted = list(req.get("grants", []))
                    with self.state_lock:
                        expected = self.perm_consents.get(ctok)
                    if expected != (pid, _digest(submitted)):
                        self._send_json(
                            409,
                            {"ok": False, "error": {"code": "plugin.consent_stale", "message": "grants_digest mismatch"}},
                        )
                        return
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        self.grants_by_pid[pid] = submitted
                        self.plugins[pid]["revision"] = int(self.plugins[pid]["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        full_reg = build_default_registry()
        f06_03 = [c for c in full_reg.cases if c.id == "T1-F06-03"][0]
        sub_reg = SuiteRegistry()
        sub_reg.register(f06_03)

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), PermissionsDigestContractHandler)
        thr = threading.Thread(target=srv.serve_forever, daemon=True)
        thr.start()
        try:
            url = f"http://127.0.0.1:{srv.server_address[1]}"
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    args = parse_args(["--target-url", url, "--tier", "1"])
                    rep = run_suite(args, registry=sub_reg)
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root
            self.assertEqual(rep["summary"]["total"], 1, rep)
            self.assertEqual(rep["summary"]["passed"], 1, rep)
            self.assertEqual(rep["summary"]["failed"], 0, rep)

            # Negative check: submitting only grant_diff.added returns 409 plugin.consent_stale
            cli = ApiClient(url)
            cli.login("admin", "e2e-admin-password-secret")
            any_pid = next(iter(PermissionsDigestContractHandler.plugins))
            full_g = list(PermissionsDigestContractHandler.grants_by_pid[any_pid])
            PermissionsDigestContractHandler.grants_by_pid[any_pid] = [full_g[0]]
            prep = cli.post(
                f"/api/v1/plugins/{any_pid}/permissions/prepare",
                json_body={"requested_grants": full_g},
            )
            self.assertEqual(len(prep["grant_diff"]["added"]), 1)
            self.assertEqual(len(prep["grant_diff"]["kept"]), 1)
            err = assert_api_error(
                lambda: cli.post(
                    f"/api/v1/plugins/{any_pid}/permissions/grant",
                    json_body={"consent_token": prep["consent_token"], "grants": prep["grant_diff"]["added"]},
                    if_match="1",
                    idempotency_key="subset-grant",
                ),
                (409,),
                label="subset grant_diff.added rejected",
            )
            self.assertEqual(err.code, "plugin.consent_stale")
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_t2_f12_02_restores_browser_disabled_on_failure(self) -> None:
        browser_calls = []

        class BrowserRestoreHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _ok(self, data: object, status: int = 200, extra_headers: list = None) -> None:
                body = json.dumps({"ok": True, "data": data}).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                if path == "/api/v1/health":
                    self._ok({"status": "ok", "read_only": False})
                elif path == "/api/v1/auth/me":
                    self._ok({"id": "u1", "login": "admin", "role": "admin", "enabled": True})
                elif path.endswith("/browser-availability"):
                    self._ok({"status": "unavailable", "missing_requirements": ["browser"]})
                else:
                    self._ok({})

            def do_POST(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                self._read()
                if path == "/api/v1/auth/login":
                    tok = f"tok-{uuid.uuid4().hex}"
                    self._ok(
                        {"token": tok, "user": {"id": "u1", "login": "admin", "role": "admin", "enabled": True}},
                        extra_headers=[("Set-Cookie", f"mc_session={tok}; Path=/; HttpOnly")],
                    )
                elif path == "/api/v1/sites":
                    self._ok({"id": str(uuid.uuid4()), "name": "s1", "profile_id": "pterclub"}, status=201)
                else:
                    self._ok({})

            def do_PUT(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/settings/browser":
                    cdp = req.get("cdp", {})
                    browser_calls.append(bool(cdp.get("enabled")))
                    self._ok({"cdp": cdp})
                else:
                    self._ok({})

        full_reg = build_default_registry()
        f12_02 = [c for c in full_reg.cases if c.id == "T2-F12-02"][0]
        sub_reg = SuiteRegistry()
        sub_reg.register(f12_02)

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), BrowserRestoreHandler)
        thr = threading.Thread(target=srv.serve_forever, daemon=True)
        thr.start()
        try:
            url = f"http://127.0.0.1:{srv.server_address[1]}"
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    args = parse_args(["--target-url", url, "--tier", "2"])
                    rep = run_suite(args, registry=sub_reg)
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root
            self.assertEqual(rep["summary"]["total"], 1, rep)
            self.assertEqual(rep["summary"]["passed"], 0, rep)
            self.assertEqual(rep["summary"]["failed"], 1, rep)
            self.assertEqual(browser_calls, [False, True, False], f"Expected browser disabled restore, got {browser_calls!r}")
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)


class TestE2EM3Tier3AndTier4(unittest.TestCase):
    def test_e2e_m3_coverage_thresholds_and_feature_matrix(self) -> None:
        reg = build_default_registry()
        stats = validate_registry_thresholds(reg)
        self.assertGreaterEqual(stats["counts_by_tier"][1], 132)
        self.assertGreaterEqual(stats["counts_by_tier"][2], 132)
        self.assertGreaterEqual(stats["non_smoke_by_tier"][3], 29)
        self.assertGreaterEqual(stats["counts_by_tier"][3], 30)
        self.assertGreaterEqual(stats["non_smoke_by_tier"][4], 13)
        self.assertGreaterEqual(stats["counts_by_tier"][4], 14)
        self.assertGreaterEqual(stats["total"], 308)
        self.assertEqual(stats["tier3_features_covered"], list(range(1, 27)))

        # Every feature #1-#26 must participate in >= 2 non-smoke Tier 3 pairwise cases
        t3_pair_counts = {f: 0 for f in range(1, 27)}
        t4_scen_ids = []
        for c in reg.cases:
            if c.tier == 3 and not c.self_test:
                for fid in {c.feature_id, *c.related_features}:
                    t3_pair_counts[fid] += 1
            if c.tier == 4 and not c.self_test:
                t4_scen_ids.append(c.id)
        for fid in range(1, 27):
            self.assertGreaterEqual(
                t3_pair_counts[fid],
                2,
                f"Feature #{fid} covered in < 2 Tier 3 pairwise cases: {t3_pair_counts[fid]}",
            )
        self.assertEqual(
            t4_scen_ids,
            [f"T4-SCEN-{i:02d}" for i in range(1, 14)],
        )

        # Negative threshold checks on synthetic incomplete registries
        bad_reg = SuiteRegistry()

        @bad_reg.case(id="BAD-T3", tier=3, feature_id=1, name="Single feature", self_test=False)
        def _bad_t3(ctx: TestContext) -> None:
            return None

        with self.assertRaises(ValueError):
            validate_registry_thresholds(bad_reg)

        def _clone_reg(skip_ids=None, mutate=None) -> SuiteRegistry:
            skip = set(skip_ids or ())
            cloned = SuiteRegistry()
            for orig in reg.cases:
                if orig.id in skip:
                    continue
                item = mutate(orig) if mutate else orig
                if item is not None:
                    cloned.register(item)
            return cloned

        # 1. Duplicate IDs
        dup_reg = _clone_reg()
        dup_reg._cases.append(reg.cases[0])
        with self.assertRaises(ValueError):
            validate_registry_thresholds(dup_reg)

        # 2. < 5 non-smoke Tier 1 padded with T1-F01-SMOKE-02 + extra Feature #2 non-smoke
        t1_f1_non_smoke = [c.id for c in reg.cases if c.tier == 1 and c.feature_id == 1 and not c.self_test]
        self.assertGreaterEqual(len(t1_f1_non_smoke), 5)
        pad_t1_reg = _clone_reg(skip_ids=[t1_f1_non_smoke[0]])
        pad_t1_reg.register(
            TestCase(id="T1-F02-PAD", tier=1, feature_id=2, name="Pad", fn=lambda ctx: None, self_test=False)
        )
        with self.assertRaises(ValueError):
            validate_registry_thresholds(pad_t1_reg)

        # 3. < 5 non-smoke Tier 2 padded with T2-F24-SMOKE-01 + extra Feature #2 non-smoke
        t2_f24_non_smoke = [c.id for c in reg.cases if c.tier == 2 and c.feature_id == 24 and not c.self_test]
        self.assertGreaterEqual(len(t2_f24_non_smoke), 5)
        pad_t2_reg = _clone_reg(skip_ids=[t2_f24_non_smoke[0]])
        pad_t2_reg.register(
            TestCase(id="T2-F02-PAD", tier=2, feature_id=2, name="Pad", fn=lambda ctx: None, self_test=False)
        )
        with self.assertRaises(ValueError):
            validate_registry_thresholds(pad_t2_reg)

        # 4. < 26 non-smoke Tier 3
        t3_non_smoke = [c.id for c in reg.cases if c.tier == 3 and not c.self_test]
        lt26_reg = _clone_reg(skip_ids=t3_non_smoke[: len(t3_non_smoke) - 25])
        with self.assertRaises(ValueError):
            validate_registry_thresholds(lt26_reg)

        # 5. Missing Feature #26 from Tier 3 non-smoke pairwise coverage
        no_f26_t3_reg = _clone_reg(
            skip_ids=[
                c.id
                for c in reg.cases
                if c.tier == 3 and not c.self_test and 26 in {c.feature_id, *c.related_features}
            ]
        )
        with self.assertRaises(ValueError):
            validate_registry_thresholds(no_f26_t3_reg)

        # 6. Single-feature Tier 3 non-smoke case
        single_t3_reg = _clone_reg(
            mutate=lambda c: TestCase(
                id=c.id, tier=c.tier, feature_id=c.feature_id, name=c.name, fn=c.fn, self_test=c.self_test, related_features=()
            )
            if c.id == "T3-P01"
            else c
        )
        with self.assertRaises(ValueError):
            validate_registry_thresholds(single_t3_reg)

        # 7. Single-feature Tier 4 non-smoke scenario
        single_t4_reg = _clone_reg(
            mutate=lambda c: TestCase(
                id=c.id, tier=c.tier, feature_id=c.feature_id, name=c.name, fn=c.fn, self_test=c.self_test, related_features=()
            )
            if c.id == "T4-SCEN-01"
            else c
        )
        with self.assertRaises(ValueError):
            validate_registry_thresholds(single_t4_reg)

        # 8. < 13 non-smoke Tier 4 scenarios
        lt13_t4_reg = _clone_reg(skip_ids=["T4-SCEN-13"])
        with self.assertRaises(ValueError):
            validate_registry_thresholds(lt13_t4_reg)

        # 9. Missing smoke tier for each tier 1..4
        for tier_num in (1, 2, 3, 4):
            no_smoke_reg = _clone_reg(skip_ids=[c.id for c in reg.cases if c.tier == tier_num and c.self_test])
            with self.assertRaises(ValueError, msg=f"Expected failure when tier {tier_num} smoke missing"):
                validate_registry_thresholds(no_smoke_reg)

    def test_e2e_m3_all_tiers_baseline_404_zero_fake_passes(self) -> None:
        BaselineHandler = _make_baseline_handler()
        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), BaselineHandler)
        thr = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thr.start()
        try:
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    url = f"http://127.0.0.1:{srv.server_address[1]}"
                    args = parse_args(["--target-url", url, "--all"])
                    report = run_suite(args, registry=build_default_registry())
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root

            summary = report["summary"]
            self.assertGreaterEqual(summary["total"], 305)
            self.assertEqual(
                summary["passed"],
                0,
                f"Fake passes on BaselineHandler: {[r['id'] for r in report['results'] if r['status'] == 'pass']}",
            )
            self.assertEqual(summary["failed"], summary["total"])
            self.assertEqual(summary["unimplemented"], summary["total"])
            for r in report["results"]:
                err_txt = r.get("error") or ""
                for bad_exc in ("NameError", "AttributeError", "TypeError", "KeyError"):
                    self.assertNotIn(bad_exc, err_txt, f"Case {r['id']} crashed with {bad_exc}: {err_txt}")
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_e2e_m3_tier3_tier4_broken_phase1_fault_injection_sweeps(self) -> None:
        for smart_mode, cli_specs in (
            (False, (("cli0", "#!/bin/sh\necho '{}'\nexit 0\n"),)),
            (
                True,
                (
                    ("cli0", "#!/bin/sh\necho '{}'\nexit 0\n"),
                    ("cli1", "#!/bin/sh\necho 'invalid plugin' >&2\nexit 1\n"),
                ),
            ),
        ):
            for cli_label, cli_body in cli_specs:
                srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), _make_sweep_broken_handler(smart=smart_mode))
                thr = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
                thr.start()
                try:
                    with tempfile.TemporaryDirectory() as isolated_tmp_root:
                        broken_cli = Path(isolated_tmp_root) / "broken_cli.sh"
                        broken_cli.write_text(cli_body, encoding="utf-8")
                        os.chmod(broken_cli, 0o755)
                        prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                        try:
                            url = f"http://127.0.0.1:{srv.server_address[1]}"
                            args = parse_args(
                                ["--target-url", url, "--bin", str(broken_cli), "--tier", "3", "--tier", "4"]
                            )
                            report = run_suite(args, registry=build_default_registry())
                        finally:
                            if prev_root is None:
                                os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                            else:
                                os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root

                    summary = report["summary"]
                    self.assertGreaterEqual(summary["total"], 41)
                    passed_ids = {r["id"] for r in report["results"] if r["status"] == "pass"}
                    if not smart_mode:
                        self.assertEqual(
                            passed_ids,
                            set(),
                            f"FullBrokenPhase1Handler ({cli_label}) vacuous passes: {sorted(passed_ids)}",
                        )
                    else:
                        self.assertTrue(
                            passed_ids <= {"T3-INFRA-01"},
                            f"SmartBrokenPhase1Handler ({cli_label}) vacuous T3/T4 passes: {sorted(passed_ids)}",
                        )
                    for r in report["results"]:
                        err_txt = r.get("error") or ""
                        for bad_exc in ("NameError", "KeyError", "AttributeError", "TypeError"):
                            self.assertNotIn(
                                bad_exc,
                                err_txt,
                                f"Case {r['id']} (smart={smart_mode}, {cli_label}) crashed: {err_txt}",
                            )
                        if r["unimplemented"]:
                            self.assertIn(
                                "InstanceSupervisor crash/restart",
                                err_txt,
                                f"Unexpected HTTP UNIMPL on {r['id']} (smart={smart_mode}, {cli_label}): {err_txt}",
                            )
                finally:
                    srv.shutdown()
                    srv.server_close()
                    thr.join(timeout=2.0)

    def test_e2e_m3_t3_t4_positive_contracts_and_cli_tier_reports(self) -> None:
        # Part A: Subprocess CLI --tier 3 / --tier 4 / --all report checks against BaselineHandler
        BaselineHandler = _make_baseline_handler()
        bsrv = _ThreadedStubHTTPServer(("127.0.0.1", 0), BaselineHandler)
        bthr = threading.Thread(target=bsrv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        bthr.start()
        try:
            burl = f"http://127.0.0.1:{bsrv.server_address[1]}"
            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                sub_env = os.environ.copy()
                sub_env["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                for cli_args, exp_tiers, min_total, rname in (
                    (["--target-url", burl, "--tier", "3"], [3], 30, "t3.json"),
                    (["--target-url", burl, "--tier", "4"], [4], 14, "t4.json"),
                    (["--target-url", burl, "--all"], [1, 2, 3, 4], 308, "all.json"),
                ):
                    rpath = Path(isolated_tmp_root) / rname
                    proc = subprocess.run(
                        [sys.executable, "scripts/e2e/plugin_suite/run.py"]
                        + cli_args
                        + ["--json-report", str(rpath)],
                        cwd=_REPO_ROOT,
                        env=sub_env,
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    self.assertEqual(proc.returncode, 1, f"Expected exit 1 for baseline {cli_args}: {proc.stderr}")
                    rep = json.loads(rpath.read_text(encoding="utf-8"))
                    self.assertEqual(rep["tiers"], exp_tiers)
                    self.assertGreaterEqual(rep["summary"]["total"], min_total)
                    self.assertEqual(rep["summary"]["passed"], 0)
                    self.assertEqual(rep["summary"]["unimplemented"], rep["summary"]["total"])
                    self.assertIn("registry_counts", rep)
                    self.assertEqual(list(Path(isolated_tmp_root).glob("crawler-e2e-*")), [])
        finally:
            bsrv.shutdown()
            bsrv.server_close()
            bthr.join(timeout=2.0)

        # Part B: Stateful T3T4ContractHandler verifying non-restart T3/T4 positive contracts + restart UNIMPL
        def _parse_pkg_upload(content_type: str, body: bytes) -> tuple:
            if "boundary=" in content_type:
                boundary = content_type.split("boundary=", 1)[1].strip().strip('"').encode("latin-1")
                for part in body.split(b"--" + boundary):
                    if b'name="file"' in part and b"\r\n\r\n" in part:
                        payload = part.split(b"\r\n\r\n", 1)[1]
                        if payload.endswith(b"\r\n"):
                            payload = payload[:-2]
                        sha = hashlib.sha256(payload).hexdigest()
                        pid = ""
                        version = "1.0.0"
                        ui_surfaces = [{"id": "cfg", "kind": "config", "expose": "./Config", "title": "Config"}]
                        http_rules = []
                        with zipfile.ZipFile(io.BytesIO(payload), "r") as zf:
                            toml_txt = zf.read("plugin.toml").decode("utf-8", errors="ignore")
                            cur_surf: dict = {}
                            cur_http: dict = {}
                            section = ""
                            for raw_line in toml_txt.splitlines():
                                ln = raw_line.strip()
                                if ln.startswith("[["):
                                    if section == "ui.surfaces" and cur_surf:
                                        ui_surfaces.append(cur_surf)
                                        cur_surf = {}
                                    if section == "http" and cur_http:
                                        http_rules.append(cur_http)
                                        cur_http = {}
                                    section = ln.strip("[]")
                                elif ln.startswith("["):
                                    section = ln.strip("[]")
                                elif "=" in ln:
                                    k, v = [x.strip() for x in ln.split("=", 1)]
                                    v_clean = v.strip('"').strip("'")
                                    if section == "" and k == "id":
                                        pid = v_clean
                                    elif section == "" and k == "version":
                                        version = v_clean
                                    elif section == "http":
                                        cur_http[k] = v_clean
                                    elif section == "ui.surfaces":
                                        cur_surf[k] = v_clean
                            if cur_http:
                                http_rules.append(cur_http)
                            if cur_surf:
                                ui_surfaces.append(cur_surf)
                            assets = {
                                name: zf.read(name)
                                for name in zf.namelist()
                                if name.startswith("ui/assets/")
                            }
                        if not any(s.get("kind") == "config" for s in ui_surfaces[1:]):
                            pass
                        else:
                            ui_surfaces = ui_surfaces[1:]
                        return sha, pid, version, ui_surfaces, http_rules, assets
            return hashlib.sha256(body).hexdigest(), "", "1.0.0", [], [], {}

        class T3T4ContractHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            state_lock = threading.Lock()
            users = {
                "admin": {"id": "00000000-0000-0000-0000-000000000001", "login": "admin", "role": "admin", "enabled": True, "password": "e2e-admin-password-secret"}
            }
            tokens = {}
            uploads = {}
            preps = {}
            ops = {}
            ui_trust = {}
            sites = {}
            downloaders = {}
            configs = {}
            checkin_done = set()
            brush_used = {}
            reauth_map = {}
            plugin_assets = {
                "builtin.checkin": {"ui/assets/remoteEntry.js": b"export const checkinRemote = true;\n"},
                "builtin.boost": {"ui/assets/remoteEntry.js": b"export const boostRemote = true;\n"},
            }
            plugin_surfaces = {
                "builtin.checkin": [
                    {"id": "cfg", "kind": "config", "title": "Config"},
                    {"id": "pg", "kind": "page", "title": "Page"},
                    {"id": "app", "kind": "app", "title": "App"},
                    {"id": "dash", "kind": "dashboard", "title": "Dashboard"},
                ],
                "builtin.boost": [
                    {"id": "cfg", "kind": "config", "title": "Config"},
                    {"id": "pg", "kind": "page", "title": "Page"},
                    {"id": "app", "kind": "app", "title": "App"},
                    {"id": "dash", "kind": "dashboard", "title": "Dashboard"},
                ],
            }
            plugin_http_rules = {}
            plugins = {
                "builtin.checkin": {
                    "id": "builtin.checkin",
                    "version": "1.0.0",
                    "revision": 1,
                    "generation": 1,
                    "lifecycle": "enabled",
                    "package_hash": "a" * 64,
                },
                "builtin.boost": {
                    "id": "builtin.boost",
                    "version": "1.0.0",
                    "revision": 1,
                    "generation": 1,
                    "lifecycle": "enabled",
                    "package_hash": "b" * 64,
                },
            }

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _resolve_user(self, cookie_only: bool = False) -> object:
                tok = None
                if not cookie_only:
                    auth = self.headers.get("Authorization", "")
                    if auth.startswith("Bearer "):
                        tok = auth[7:].strip()
                if not tok:
                    ck = self.headers.get("Cookie", "")
                    for part in ck.split(";"):
                        if "=" in part:
                            k, v = part.strip().split("=", 1)
                            if k == "mc_session":
                                tok = v.strip()
                                break
                with self.state_lock:
                    login = self.tokens.get(tok) if tok else None
                    return dict(self.users[login]) if login and login in self.users else None

            def _send_bytes(self, status: int, content_type: str, body: bytes, extra_headers: list = None) -> None:
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                if extra_headers:
                    for k, v in extra_headers:
                        self.send_header(k, v)
                self.end_headers()
                if self.command != "HEAD":
                    self.wfile.write(body)

            def _send_json(self, status: int, payload: object, extra_headers: list = None) -> None:
                self._send_bytes(status, "application/json", json.dumps(payload).encode("utf-8"), extra_headers=extra_headers)

            def _ok(self, data: object, status: int = 200, extra_headers: list = None) -> None:
                self._send_json(status, {"ok": True, "data": data}, extra_headers=extra_headers)

            def _require_admin(self) -> object:
                u = self._resolve_user()
                if u is None:
                    self._send_json(401, {"ok": False, "error": {"code": "auth.required", "message": "auth required"}})
                    return None
                if u.get("role") != "admin":
                    self._send_json(403, {"ok": False, "error": {"code": "auth.forbidden", "message": "admin required"}})
                    return None
                return u

            def do_GET(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                path = parsed.path
                if path == "/api/v1/health":
                    self._ok({"status": "ok", "read_only": False})
                elif path == "/api/v1/auth/me":
                    u = self._resolve_user()
                    if u is None:
                        self._send_json(401, {"ok": False, "error": {"code": "auth.required", "message": "auth required"}})
                    else:
                        self._ok({"id": u["id"], "login": u["login"], "role": u["role"], "enabled": True})
                elif path.startswith("/plugins/"):
                    self._send_json(404, {"error": "not found"})
                elif path.startswith("/plugin-apps/"):
                    self._send_bytes(200, "text/html; charset=utf-8", b"<!doctype html><html><body><div id='root'></div></body></html>")
                elif path == "/api/v1/sites/boost-stats":
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "removed"}})
                elif path.startswith("/api/v1/plugin-ui-assets/"):
                    u = self._resolve_user(cookie_only=True)
                    if u is None:
                        self._send_json(401, {"ok": False, "error": {"code": "auth.required", "message": "cookie required"}})
                        return
                    if u.get("role") != "admin":
                        self._send_json(403, {"ok": False, "error": {"code": "plugin.forbidden", "message": "admin required"}})
                        return
                    parts = path.split("/")
                    pid, phash = parts[4], parts[5]
                    rel_path = "/".join(parts[6:])
                    with self.state_lock:
                        p = self.plugins.get(pid)
                        assets = self.plugin_assets.get(pid, {})
                        trusted_hash = self.ui_trust.get(pid, {}).get("hash")
                    if p is None or trusted_hash != phash:
                        self._send_json(403, {"ok": False, "error": {"code": "plugin.ui_untrusted", "message": "untrusted"}})
                        return
                    body = assets.get(rel_path)
                    if body is None:
                        self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "missing asset"}})
                        return
                    self._send_bytes(
                        200,
                        "application/javascript; charset=utf-8",
                        body,
                        extra_headers=[("X-Content-Type-Options", "nosniff"), ("Cache-Control", "private, no-store")],
                    )
                elif path == "/api/v1/plugin-ui-navigation":
                    if self._require_admin() is None:
                        return
                    with self.state_lock:
                        nav = [
                            {"plugin_id": pid, "surfaces": self.plugin_surfaces.get(pid, [])}
                            for pid, p in self.plugins.items()
                            if p["lifecycle"] == "enabled"
                        ]
                    self._ok(nav)
                elif path == "/api/v1/plugins":
                    if self._require_admin() is None:
                        return
                    with self.state_lock:
                        self._ok(list(self.plugins.values()))
                elif path.startswith("/api/v1/plugins/") and path.endswith("/ui"):
                    u = self._require_admin()
                    if u is None:
                        return
                    pid = path.split("/")[4]
                    with self.state_lock:
                        p = self.plugins[pid]
                        tr = self.ui_trust.get(pid, {"status": "untrusted", "revision": 0, "hash": None})
                        is_enabled = p["lifecycle"] == "enabled"
                        is_trusted = tr["status"] == "trusted" and tr["hash"] == p["package_hash"]
                        surfs = [
                            {
                                "id": s.get("id", "s"),
                                "kind": s.get("kind", "config"),
                                "editable": s.get("kind") == "config" and not is_enabled,
                                "mountable": is_enabled and is_trusted,
                            }
                            for s in self.plugin_surfaces.get(pid, [])
                        ]
                        self._ok(
                            {
                                "subject_user_id": u["id"],
                                "plugin_id": pid,
                                "package_hash": p["package_hash"],
                                "entry_url": f"/api/v1/plugin-ui-assets/{pid}/{p['package_hash']}/ui/assets/remoteEntry.js",
                                "contract": "1.0.0",
                                "react": "19.3.0",
                                "surfaces": surfs,
                                "trust": {"status": "trusted" if is_trusted else "untrusted", "revision": tr["revision"]},
                            }
                        )
                elif path.startswith("/api/v1/plugins/") and path.endswith("/config"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        cfg = self.configs.setdefault(pid, {"plugin_id": pid, "revision": 1, "schema_revision": "1", "values": {}, "secrets": {}})
                    self._ok(cfg)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/brush-state"):
                    qs = urllib.parse.parse_qs(parsed.query)
                    key = (qs.get("site_id", [""])[0], qs.get("downloader_id", [""])[0])
                    with self.state_lock:
                        used = self.brush_used.get(key, 0)
                    self._ok({"used_bytes": used, "reserved_bytes": 0, "retained_file_bytes": 0, "tasks": [], "disk": {"status": "available", "available_bytes": 100000000}})
                elif path.startswith("/api/v1/plugins/"):
                    pid = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        p = self.plugins.get(pid)
                    self._ok(p)
                elif path.startswith("/api/v1/plugin-operations/"):
                    op_id = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        op = self.ops.get(op_id, {"operation_id": op_id, "state": "succeeded"})
                    self._ok(op)
                elif path.startswith("/api/v1/sites/") and path.endswith("/plugin-status"):
                    sid = path.split("/")[4]
                    with self.state_lock:
                        s = self.sites[sid]
                        done = sid in self.checkin_done
                    receipts = [{"action": "check-in", "status": "completed"}] if done else []
                    self._ok({"credential_epoch": s["credential_epoch"], "receipts": receipts})
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_POST(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                path = parsed.path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/auth/login":
                    uname = req.get("username", "")
                    with self.state_lock:
                        u = self.users.get(uname)
                        if not u or u.get("password") != req.get("password"):
                            self._send_json(401, {"ok": False, "error": {"code": "auth.invalid", "message": "invalid"}})
                            return
                        tok = f"t3t4-{uuid.uuid4().hex}"
                        self.tokens[tok] = uname
                        uview = {"id": u["id"], "login": u["login"], "role": u["role"], "enabled": True}
                    self._ok({"token": tok, "user": uview}, extra_headers=[("Set-Cookie", f"mc_session={tok}; Path=/; HttpOnly")])
                elif path == "/api/v1/users":
                    uid = str(uuid.uuid4())
                    uname = req["login"]
                    with self.state_lock:
                        self.users[uname] = {"id": uid, "login": uname, "role": "member", "enabled": True, "password": req["password"]}
                    self._ok({"id": uid, "login": uname, "role": "member", "enabled": True}, status=201)
                elif path == "/api/v1/plugin-reauth":
                    purpose = req.get("purpose", "")
                    tok = f"reauth-{uuid.uuid4().hex}"
                    with self.state_lock:
                        self.reauth_map[tok] = purpose
                    self._ok({"reauth_token": tok, "purpose": purpose})
                elif path == "/api/v1/plugin-trust-keys/prepare":
                    self._ok({"key_id": "0" * 16, "fingerprint": "f" * 64, "consent_token": "c-tk"})
                elif path == "/api/v1/plugin-trust-keys":
                    self._ok({"id": str(uuid.uuid4()), "key_id": "0" * 16, "status": "trusted", "revision": 1}, status=201)
                elif path == "/api/v1/plugin-uploads":
                    uid = str(uuid.uuid4())
                    sha, pid, ver, surfs, http_rules, assets = _parse_pkg_upload(self.headers.get("Content-Type", ""), raw)
                    with self.state_lock:
                        self.uploads[uid] = (sha, pid, ver, surfs, http_rules, assets)
                    self._ok({"upload_id": uid, "hash": sha, "signature_status": "verified"}, status=201)
                elif path == "/api/v1/plugin-installations/prepare":
                    op_id = str(uuid.uuid4())
                    prep_id = f"prep-{op_id}"
                    sha, pid, ver, surfs, http_rules, assets = self.uploads[req["upload_id"]]
                    with self.state_lock:
                        self.preps[prep_id] = (sha, pid, ver, surfs, http_rules, assets)
                        self.ops[op_id] = {
                            "operation_id": op_id,
                            "state": "succeeded",
                            "result": {
                                "preparation_id": prep_id,
                                "consent_token": f"c-{op_id}",
                                "package_hash": sha,
                                "grant_diff": {"added": [{"capability": "kv", "scope": "Global", "sensitive": False}], "kept": []},
                            },
                        }
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/plugin-installations":
                    op_id = str(uuid.uuid4())
                    sha, pid, ver, surfs, http_rules, assets = self.preps[req["preparation_id"]]
                    with self.state_lock:
                        self.plugins[pid] = {
                            "id": pid,
                            "version": ver,
                            "revision": 1,
                            "generation": 0,
                            "lifecycle": "installed-disabled",
                            "package_hash": sha,
                        }
                        self.plugin_surfaces[pid] = surfs
                        self.plugin_http_rules[pid] = http_rules
                        self.plugin_assets[pid] = assets
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/upgrades/prepare"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    prep_id = f"prep-{op_id}"
                    sha, _, ver, surfs, http_rules, assets = self.uploads[req["upload_id"]]
                    with self.state_lock:
                        self.preps[prep_id] = (sha, pid, ver, surfs, http_rules, assets)
                        self.ops[op_id] = {
                            "operation_id": op_id,
                            "state": "succeeded",
                            "result": {
                                "preparation_id": prep_id,
                                "consent_token": f"c-{op_id}",
                                "package_hash": sha,
                                "config_diff": {"target_config_digest": "d" * 64},
                                "grant_diff": {"added": [], "kept": [{"capability": "kv", "scope": "Global", "sensitive": False}]},
                            },
                        }
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/upgrade"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    sha, _, ver, surfs, http_rules, assets = self.preps[req["preparation_id"]]
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["version"] = ver
                        p["package_hash"] = sha
                        p["revision"] = int(p["revision"]) + 1
                        self.plugin_surfaces[pid] = surfs
                        self.plugin_http_rules[pid] = http_rules
                        self.plugin_assets[pid] = assets
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/enable"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["lifecycle"] = "enabled"
                        p["generation"] = int(p["generation"]) + 1
                        p["revision"] = int(p["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/disable"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["lifecycle"] = "disabled"
                        p["generation"] = int(p["generation"]) + 1
                        p["revision"] = int(p["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/uninstall"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        self.plugins.pop(pid, None)
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/ui-trust/prepare"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        rev = self.ui_trust.get(pid, {"revision": 0})["revision"]
                    self._ok({"consent_token": f"c-ui-{pid}", "trust_revision": rev})
                elif path.startswith("/api/v1/plugins/") and path.endswith("/ui-trust/grant"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        prev_rev = self.ui_trust.get(pid, {"revision": 0})["revision"]
                        self.ui_trust[pid] = {"status": "trusted", "revision": prev_rev + 1, "hash": req["package_hash"]}
                    self._ok({"status": "trusted", "revision": prev_rev + 1})
                elif path.startswith("/api/v1/plugins/") and path.endswith("/ui-trust/revoke"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        prev_rev = self.ui_trust.get(pid, {"revision": 0})["revision"]
                        self.ui_trust[pid] = {"status": "revoked", "revision": prev_rev + 1, "hash": None}
                    self._ok({"status": "revoked", "revision": prev_rev + 1})
                elif path.startswith("/api/v1/plugins/") and path.endswith("/ui/query"):
                    qname = req.get("name")
                    qinput = req.get("input") or {}
                    if qname == "handles.resolve":
                        self._ok({"handle": f"opaque-{uuid.uuid4().hex[:8]}"})
                    elif qname == "checkin.receipts":
                        self._ok({"items": []})
                    elif qname == "boost.snapshot":
                        key = (qinput.get("site_id", ""), qinput.get("downloader_id", ""))
                        with self.state_lock:
                            used = self.brush_used.get(key, 0)
                        self._ok({"used_bytes": used, "reserved_bytes": 0, "retained_file_bytes": 0})
                    else:
                        self._ok({"items": []})
                elif path.startswith("/api/v1/plugins/") and path.endswith("/commands"):
                    pid = path.split("/")[4]
                    hdr_gen = self.headers.get("X-Plugin-Generation")
                    with self.state_lock:
                        p = self.plugins[pid]
                        if hdr_gen is not None and str(p["generation"]) != str(hdr_gen):
                            self._send_json(409, {"ok": False, "error": {"code": "plugin.stale_generation", "message": "stale generation"}})
                            return
                    op_id = str(uuid.uuid4())
                    kind = req.get("kind")
                    payload = req.get("payload") or {}
                    if kind == "site.maintenance":
                        sid = payload["site_id"]
                        if sid not in self.checkin_done:
                            s = self.sites[sid]
                            with ApiClient(s["url"]) as site_cli:
                                site_cli.raw_request("POST", "/attendance.php", headers={"Cookie": s["cookie"]})
                            self.checkin_done.add(sid)
                            for plg_id in list(self.plugins.keys()):
                                for rule in self.plugin_http_rules.get(plg_id, []):
                                    orig = rule.get("origin")
                                    if orig:
                                        with ApiClient(orig) as wh_cli:
                                            wh_cli.raw_request(
                                                "POST",
                                                "/webhook/events",
                                                body=json.dumps({"id": str(uuid.uuid4()), "event": "site.checkin.after"}).encode("utf-8"),
                                                headers={"Content-Type": "application/json"},
                                            )
                    elif kind == "downloader.add":
                        dl_url = payload["download_url"]
                        parts = urllib.parse.urlsplit(dl_url)
                        with ApiClient(f"{parts.scheme}://{parts.netloc}") as t_cli:
                            t_bytes = t_cli.raw_request("GET", parts.path).body
                        decoded = bencode_decode(t_bytes)
                        info = decoded[b"info"]
                        total_sz = int(info[b"length"]) if b"length" in info else sum(int(f[b"length"]) for f in info[b"files"])
                        dl = self.downloaders[payload["downloader_id"]]
                        with ApiClient(dl["url"]) as qb_cli:
                            qb_cli.post_multipart(
                                "/api/v2/torrents/add",
                                fields={"tags": "crawler-media"},
                                files=[("torrents", "t.torrent", t_bytes, "application/x-bittorrent")],
                                expect_envelope=False,
                            )
                        key = (payload["site_id"], payload["downloader_id"])
                        with self.state_lock:
                            self.brush_used[key] = self.brush_used.get(key, 0) + total_sz
                    with self.state_lock:
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/downloaders":
                    did = str(uuid.uuid4())
                    dl = {"id": did, "url": req["url"], "name": req.get("name", "qb")}
                    with self.state_lock:
                        self.downloaders[did] = dl
                    self._ok(dl, status=201)
                elif path == "/api/v1/sites":
                    sid = str(uuid.uuid4())
                    s = {"id": sid, "url": req["url"], "cookie": req.get("cookie", ""), "credential_epoch": str(uuid.uuid4())}
                    with self.state_lock:
                        self.sites[sid] = s
                    self._ok(s, status=201)
                elif path.startswith("/api/v1/sites/") and path.endswith("/plugin-actions"):
                    sid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    action = req.get("action")
                    already = sid in self.checkin_done
                    if action == "check-in" and not already:
                        s = self.sites[sid]
                        with ApiClient(s["url"]) as site_cli:
                            site_cli.raw_request("POST", "/attendance.php", headers={"Cookie": s["cookie"]})
                        self.checkin_done.add(sid)
                    with self.state_lock:
                        self.ops[op_id] = {
                            "operation_id": op_id,
                            "state": "succeeded",
                            "result": {"status": "already-completed" if already else "completed"},
                        }
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/sites/") and path.endswith("/verify"):
                    sid = path.split("/")[4]
                    s = self.sites[sid]
                    with ApiClient(s["url"]) as site_cli:
                        r = site_cli.raw_request("GET", "/index.php", headers={"Cookie": s["cookie"]})
                    self._ok({"ok": r.status == 200})
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_PUT(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path.startswith("/api/v1/plugins/") and path.endswith("/config"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        p = self.plugins[pid]
                        if p["lifecycle"] == "enabled":
                            self._send_json(409, {"ok": False, "error": {"code": "plugin.not_drained", "message": "enabled"}})
                            return
                        cfg = self.configs.setdefault(pid, {"plugin_id": pid, "revision": 1, "schema_revision": "1", "values": {}, "secrets": {}})
                        cfg["revision"] = int(cfg["revision"]) + 1
                        cfg["values"] = req.get("values", {})
                    self._ok(cfg)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_PATCH(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path.startswith("/api/v1/sites/") and path.endswith("/boost"):
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "removed"}})
                elif path.startswith("/api/v1/sites/"):
                    sid = path.split("/")[4]
                    with self.state_lock:
                        s = self.sites[sid]
                        if "cookie" in req or "url" in req:
                            if "cookie" in req:
                                s["cookie"] = req["cookie"]
                            if "url" in req:
                                s["url"] = req["url"]
                            s["credential_epoch"] = str(uuid.uuid4())
                    self._ok({"id": sid})
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        csrv = _ThreadedStubHTTPServer(("127.0.0.1", 0), T3T4ContractHandler)
        cthr = threading.Thread(target=csrv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        cthr.start()
        try:
            curl = f"http://127.0.0.1:{csrv.server_address[1]}"
            full_reg = build_default_registry()
            positive_ids = ("T2-F11-02", "T3-P11", "T3-P24", "T3-P25", "T3-P26", "T4-SCEN-06", "T4-SCEN-12")
            pos_reg = SuiteRegistry()
            for c in full_reg.cases:
                if c.id in positive_ids:
                    pos_reg.register(c)

            with tempfile.TemporaryDirectory() as isolated_tmp_root:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = isolated_tmp_root
                try:
                    rep = run_suite(parse_args(["--target-url", curl, "--tier", "2", "--tier", "3", "--tier", "4"]), registry=pos_reg)
                    self.assertEqual(rep["summary"]["total"], len(positive_ids), rep)
                    self.assertEqual(rep["summary"]["passed"], len(positive_ids), rep)
                    self.assertEqual(rep["summary"]["failed"], 0, rep)

                    # Restart-dependent T4-SCEN-03 under --target-url must report UNIMPL cleanly
                    rst_reg = SuiteRegistry()
                    for c in full_reg.cases:
                        if c.id == "T4-SCEN-03":
                            rst_reg.register(c)
                    rst_rep = run_suite(parse_args(["--target-url", curl, "--tier", "4"]), registry=rst_reg)
                    self.assertEqual(rst_rep["summary"]["total"], 1)
                    self.assertEqual(rst_rep["summary"]["passed"], 0)
                    self.assertEqual(rst_rep["summary"]["unimplemented"], 1)
                    self.assertIn("InstanceSupervisor crash/restart", rst_rep["results"][0]["error"])
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root
        finally:
            csrv.shutdown()
            csrv.server_close()
            cthr.join(timeout=2.0)

    def test_e2e_m3_rewrite_toml_fields_top_level_scoping(self) -> None:
        sample = (
            'schema = "crawler-plugin-v1"\n'
            'id = "org.example.scaffolded"\n'
            'name = "Scaffolded Plugin"\n'
            'version = "0.1.0"\n'
            'api_version = "1.0.0"\n'
            "\n"
            "[[components]]\n"
            'id = "hook-comp"\n'
            'version = "0.1.0"\n'
            'world = "hook-plugin"\n'
            "\n"
            "[[components.hooks]]\n"
            'id = "search.after"\n'
            "priority = 400\n"
            "\n"
            "[[components.tasks]]\n"
            'id = "sync-task"\n'
            'version = "0.1.0"\n'
            "\n"
            "[[ui.surfaces]]\n"
            'id = "cfg"\n'
            'kind = "config"\n'
            'expose = "./Config"\n'
        )
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "plugin.toml"
            p.write_text(sample, encoding="utf-8")
            _rewrite_toml_fields(p, {"id": "org.example.updated", "version": "2.3.4", "author": "e2e"})
            out = p.read_text(encoding="utf-8")

        self.assertIn('id = "org.example.updated"', out)
        self.assertIn('version = "2.3.4"', out)
        self.assertIn('author = "e2e"', out)
        # Missing key must be inserted in top-level table before first section header
        self.assertLess(out.index('author = "e2e"'), out.index("[[components]]"))
        # Nested [[components]], [[components.hooks]], [[components.tasks]], [[ui.surfaces]] must remain untouched
        self.assertIn('[[components]]\nid = "hook-comp"\nversion = "0.1.0"', out)
        self.assertIn('[[components.hooks]]\nid = "search.after"', out)
        self.assertIn('[[components.tasks]]\nid = "sync-task"\nversion = "0.1.0"', out)
        self.assertIn('[[ui.surfaces]]\nid = "cfg"', out)
        self.assertEqual(out.count('id = "org.example.updated"'), 1)
        self.assertEqual(out.count('version = "2.3.4"'), 1)

        # Also test file with no sections at all inserts missing key at EOF
        with tempfile.TemporaryDirectory() as td:
            p2 = Path(td) / "flat.toml"
            p2.write_text('id = "org.example.flat"\n', encoding="utf-8")
            _rewrite_toml_fields(p2, {"id": "org.example.flat2", "version": "1.2.3"})
            out2 = p2.read_text(encoding="utf-8")
        self.assertEqual(out2, 'id = "org.example.flat2"\nversion = "1.2.3"\n')

    def test_e2e_m3_ensure_plugin_enabled_and_lifecycle_noop_guard(self) -> None:
        noop_state = {"payload": {}, "get_valid_plugin": False, "reauth_valid": False}

        class NoopPluginHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _send_ok(self, data: object, status: int = 200) -> None:
                body = json.dumps({"ok": True, "data": data}).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                if noop_state["get_valid_plugin"] and path.startswith("/api/v1/plugins/"):
                    self._send_ok(
                        {"id": "builtin.boost", "revision": 1, "lifecycle": "installed-disabled", "generation": 0}
                    )
                    return
                if path.startswith("/api/v1/plugin-operations/"):
                    self._send_ok({"operation_id": "op-1", "state": "succeeded"})
                    return
                self._send_ok(noop_state["payload"])

            def do_POST(self) -> None:
                self._read()
                path = urllib.parse.urlsplit(self.path).path
                if noop_state["reauth_valid"] and path == "/api/v1/plugin-reauth":
                    self._send_ok({"reauth_token": "tok-noop", "purpose": "plugin:upload"})
                    return
                self._send_ok(noop_state["payload"])

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), NoopPluginHandler)
        thr = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thr.start()
        try:
            url = f"http://127.0.0.1:{srv.server_address[1]}"
            pkg = build_cmplugin(plugin_id="org.example.noopguard", kind="hook")
            with tempfile.TemporaryDirectory() as td:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = td
                try:
                    ctx = TestContext(url, ApiClient(url), "pw", "tok")
                    try:
                        for payload in ({}, [], None):
                            noop_state["payload"] = payload
                            noop_state["get_valid_plugin"] = False
                            noop_state["reauth_valid"] = False
                            for fn in (
                                lambda: ensure_plugin_enabled(ctx, "builtin.boost"),
                                lambda: enable_plugin(ctx, "builtin.boost"),
                                lambda: disable_plugin(ctx, "builtin.boost"),
                                lambda: obtain_reauth(ctx),
                            ):
                                with self.assertRaises(AssertionError) as cm:
                                    fn()
                                self.assertNotIsInstance(cm.exception, (KeyError, AttributeError, TypeError))

                            # Direct POST /enable (revision=1) & POST /disable when POST returns {}, [], None
                            noop_state["get_valid_plugin"] = True
                            for fn in (
                                lambda: enable_plugin(ctx, "builtin.boost", revision=1),
                                lambda: disable_plugin(ctx, "builtin.boost"),
                            ):
                                with self.assertRaises(AssertionError) as cm:
                                    fn()
                                self.assertNotIsInstance(cm.exception, (KeyError, AttributeError, TypeError))

                            # prepare_and_install_plugin when upload or prepare returns {}, [], None
                            noop_state["reauth_valid"] = True
                            with self.assertRaises(AssertionError) as cm:
                                prepare_and_install_plugin(ctx, pkg)
                            self.assertNotIsInstance(cm.exception, (KeyError, AttributeError, TypeError))
                    finally:
                        ctx.close()
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_e2e_m3_apiclient_keepalive_and_eaddrnotavail_resilience(self) -> None:
        client_ports = []
        get_attempts = 0
        post_attempts = 0

        class KeepAliveHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format: str, *args: object) -> None:
                return

            def do_GET(self) -> None:
                nonlocal get_attempts
                client_ports.append(self.client_address[1])
                if self.path == "/drop-first-get":
                    get_attempts += 1
                    if get_attempts == 1:
                        self.close_connection = True
                        self.connection.close()
                        return
                body = json.dumps({"ok": True, "data": {"port": self.client_address[1]}}).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self) -> None:
                nonlocal post_attempts
                length = int(self.headers.get("Content-Length", "0") or "0")
                if length > 0:
                    self.rfile.read(length)
                client_ports.append(self.client_address[1])
                if self.path == "/drop-post":
                    post_attempts += 1
                    self.close_connection = True
                    self.connection.close()
                    return
                body = json.dumps({"ok": True, "data": {"ok": True}}).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), KeepAliveHandler)
        thr = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thr.start()
        try:
            url = f"http://127.0.0.1:{srv.server_address[1]}"
            with ApiClient(url) as cli:
                p1 = cli.get("/a")["port"]
                p2 = cli.get("/b")["port"]
                p3 = cli.get("/c")["port"]
                self.assertEqual(p1, p2)
                self.assertEqual(p2, p3)

                # Explicit Connection: close must use a dedicated non-pooled socket
                r_close = cli.raw_request("GET", "/d", headers={"Connection": "close"})
                self.assertEqual(r_close.status, 200)
                self.assertNotEqual(r_close.json()["data"]["port"], p1)
                # Cached keep-alive socket remains intact for subsequent requests
                self.assertEqual(cli.get("/e")["port"], p1)

                # Stale-socket GET recovers transparently with 1 reconnect
                self.assertEqual(cli.get("/drop-first-get")["port"] > 0, True)
                self.assertEqual(get_attempts, 2)

                # Non-idempotent POST drop MUST NOT be silently retried
                with self.assertRaises(
                    (http.client.HTTPException, ConnectionResetError, ConnectionAbortedError, BrokenPipeError, EOFError)
                ):
                    cli.post("/drop-post", json_body={"x": 1})
                self.assertEqual(post_attempts, 1)

                # Bounded EADDRNOTAVAIL (Errno 49) retry recovers cleanly
                cli.close()
                real_new = cli._new_connection
                eaddr_calls = 0

                def _flaky_new_conn(timeout=None):
                    nonlocal eaddr_calls
                    eaddr_calls += 1
                    conn = real_new(timeout=timeout)
                    if eaddr_calls <= 2:
                        def _fail_req(*args, **kwargs):
                            raise OSError(errno.EADDRNOTAVAIL, "Can't assign requested address")

                        conn.request = _fail_req  # type: ignore[method-assign]
                    return conn

                cli._new_connection = _flaky_new_conn  # type: ignore[method-assign]
                res = cli.get("/after-eaddr")
                self.assertGreater(res["port"], 0)
                self.assertEqual(eaddr_calls, 3)
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_e2e_m3_pre_try_fail_at_enable_cleanup_and_devmode_202(self) -> None:
        fail_config = {"fail_on_enable_count": 1, "fail_dev_get_after_enable": False}

        class FailAtEnableHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            state_lock = threading.Lock()
            uploads = {}
            preps = {}
            ops = {}
            plugins = {}
            enable_attempts = 0
            browser_enabled = False
            dev_mode = True
            dev_rev = 1
            dev_disable_polled = False
            dev_enable_called = False
            inject_next_dev_get_500 = False

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _send_json(self, status: int, payload: object) -> None:
                body = json.dumps(payload).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def _ok(self, data: object, status: int = 200) -> None:
                self._send_json(status, {"ok": True, "data": data})

            def do_GET(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                if path == "/api/v1/health":
                    self._ok({"status": "ok", "read_only": False})
                elif path == "/api/v1/auth/me":
                    self._ok({"id": "00000000-0000-0000-0000-000000000001", "login": "admin", "role": "admin", "enabled": True})
                elif path == "/api/v1/settings/browser":
                    self._ok({"cdp": {"enabled": self.browser_enabled}})
                elif path == "/api/v1/settings/plugins":
                    with self.state_lock:
                        if FailAtEnableHandler.inject_next_dev_get_500:
                            FailAtEnableHandler.inject_next_dev_get_500 = False
                            self._send_json(500, {"ok": False, "error": {"code": "inject.fail", "message": "injected get 500"}})
                            return
                        dm = FailAtEnableHandler.dev_mode
                        dr = FailAtEnableHandler.dev_rev
                    self._ok({"developer_mode": dm, "revision": dr})
                elif path == "/api/v1/plugins":
                    with self.state_lock:
                        self._ok(list(self.plugins.values()))
                elif path.startswith("/api/v1/plugins/"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        p = self.plugins.get(pid)
                    if p is None:
                        self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "missing"}})
                    else:
                        self._ok(p)
                elif path.startswith("/api/v1/plugin-operations/"):
                    op_id = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        op = self.ops.get(op_id, {"operation_id": op_id, "state": "succeeded"})
                        if op.get("kind") == "dev_disable":
                            FailAtEnableHandler.dev_disable_polled = True
                            FailAtEnableHandler.dev_mode = False
                        elif op.get("kind") == "dev_enable":
                            FailAtEnableHandler.dev_mode = True
                    self._ok(op)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_POST(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/auth/login":
                    self._ok({"token": "tok-1", "user": {"id": "u1", "login": "admin", "role": "admin"}})
                elif path == "/api/v1/plugin-reauth":
                    self._ok({"reauth_token": "r-1", "purpose": req.get("purpose", "")})
                elif path == "/api/v1/plugin-trust-keys/prepare":
                    self._ok({"key_id": "0" * 16, "fingerprint": "f" * 64, "consent_token": "c-tk"})
                elif path == "/api/v1/plugin-trust-keys":
                    self._ok({"id": str(uuid.uuid4()), "key_id": "0" * 16, "status": "trusted", "revision": 1}, status=201)
                elif path == "/api/v1/plugin-uploads":
                    if b'name="signature"' not in raw and not FailAtEnableHandler.dev_mode:
                        self._send_json(403, {"ok": False, "error": {"code": "plugin.unsigned", "message": "unsigned rejected"}})
                        return
                    uid = str(uuid.uuid4())
                    ct = self.headers.get("Content-Type", "")
                    sha = ""
                    pid = ""
                    if "boundary=" in ct:
                        b = ct.split("boundary=", 1)[1].strip().strip('"').encode("latin-1")
                        for part in raw.split(b"--" + b):
                            if b'name="file"' in part and b"\r\n\r\n" in part:
                                pay = part.split(b"\r\n\r\n", 1)[1]
                                if pay.endswith(b"\r\n"):
                                    pay = pay[:-2]
                                sha = hashlib.sha256(pay).hexdigest()
                                with zipfile.ZipFile(io.BytesIO(pay), "r") as zf:
                                    for ln in zf.read("plugin.toml").decode("utf-8").splitlines():
                                        if ln.strip().startswith("id ="):
                                            pid = ln.split("=", 1)[1].strip().strip('"')
                                            break
                    with self.state_lock:
                        self.uploads[uid] = (sha, pid)
                    self._ok({"upload_id": uid, "hash": sha, "signature_status": "verified"}, status=201)
                elif path == "/api/v1/plugin-installations/prepare":
                    op_id = str(uuid.uuid4())
                    prep_id = f"prep-{op_id}"
                    sha, pid = self.uploads[req["upload_id"]]
                    with self.state_lock:
                        self.preps[prep_id] = (sha, pid)
                        self.ops[op_id] = {
                            "operation_id": op_id,
                            "state": "succeeded",
                            "result": {"preparation_id": prep_id, "consent_token": "c1", "package_hash": sha, "grant_diff": {"added": [], "kept": []}},
                        }
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/plugin-installations":
                    op_id = str(uuid.uuid4())
                    sha, pid = self.preps[req["preparation_id"]]
                    with self.state_lock:
                        self.plugins[pid] = {"id": pid, "revision": 1, "generation": 0, "lifecycle": "installed-disabled", "package_hash": sha}
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/enable"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        self.enable_attempts += 1
                        if self.enable_attempts == fail_config["fail_on_enable_count"]:
                            self._send_json(500, {"ok": False, "error": {"code": "inject.fail", "message": "injected enable 500"}})
                            return
                        p = self.plugins[pid]
                        p["lifecycle"] = "enabled"
                        p["generation"] = int(p["generation"]) + 1
                        p["revision"] = int(p["revision"]) + 1
                        op_id = str(uuid.uuid4())
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/disable"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["lifecycle"] = "disabled"
                        p["revision"] = int(p["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/uninstall"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        self.plugins.pop(pid, None)
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/settings/plugins/developer/enable":
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        FailAtEnableHandler.dev_enable_called = True
                        FailAtEnableHandler.dev_mode = True
                        FailAtEnableHandler.dev_rev += 1
                        FailAtEnableHandler.inject_next_dev_get_500 = bool(fail_config["fail_dev_get_after_enable"])
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded", "kind": "dev_enable"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/settings/plugins/developer/disable":
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        FailAtEnableHandler.dev_rev += 1
                        self.ops[op_id] = {"operation_id": op_id, "state": "succeeded", "kind": "dev_disable"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/libraries":
                    self._ok({"id": str(uuid.uuid4()), "name": "lib", "kind": "movie", "root_paths": req.get("root_paths", [])}, status=201)
                elif path == "/api/v1/subscriptions":
                    self._ok({"id": str(uuid.uuid4()), "library_id": req.get("library_id"), "status": "active"}, status=201)
                elif path == "/api/v1/downloaders/submit":
                    self._send_json(400, {"ok": False, "error": {"code": "net.ssrf_blocked", "message": "blocked"}})
                elif path == "/api/v1/downloaders":
                    self._ok({"id": str(uuid.uuid4()), "name": "qb", "url": req.get("url")}, status=201)
                elif path == "/api/v1/sites":
                    self._ok({"id": str(uuid.uuid4()), "name": "site", "url": req.get("url")}, status=201)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_PUT(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/settings/browser":
                    FailAtEnableHandler.browser_enabled = bool((req.get("cdp") or {}).get("enabled", False))
                    self._ok({"cdp": {"enabled": FailAtEnableHandler.browser_enabled}})
                elif path.startswith("/api/v1/libraries/") and path.endswith("/default"):
                    self._ok({"default": True})
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), FailAtEnableHandler)
        thr = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thr.start()
        try:
            url = f"http://127.0.0.1:{srv.server_address[1]}"
            full_reg = build_default_registry()
            by_id = {c.id: c for c in full_reg.cases}
            with tempfile.TemporaryDirectory() as td:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = td
                try:
                    for cid, fail_idx in (
                        ("T3-P11", 2),
                        ("T3-P15", 1),
                        ("T3-P15", 2),
                        ("T3-P15", 3),
                        ("T3-P21", 1),
                        ("T4-SCEN-06", 2),  # 1st is builtin.checkin GET; 2nd is hook_pid enable
                        ("T4-SCEN-10", 1),
                        ("T4-SCEN-13", 1),
                        ("T3-P12", 1),
                        ("T3-P13", 1),
                        ("T4-SCEN-07", 1),
                    ):
                        FailAtEnableHandler.plugins.clear()
                        FailAtEnableHandler.enable_attempts = 0
                        FailAtEnableHandler.browser_enabled = False
                        if cid == "T4-SCEN-06":
                            FailAtEnableHandler.plugins["builtin.checkin"] = {
                                "id": "builtin.checkin",
                                "revision": 1,
                                "generation": 1,
                                "lifecycle": "enabled",
                            }
                            fail_config["fail_on_enable_count"] = 1
                        elif cid == "T3-P15":
                            FailAtEnableHandler.plugins["builtin.boost"] = {
                                "id": "builtin.boost",
                                "revision": 1,
                                "generation": 1,
                                "lifecycle": "enabled",
                            }
                            fail_config["fail_on_enable_count"] = fail_idx
                        else:
                            fail_config["fail_on_enable_count"] = fail_idx
                        with TestContext(url, ApiClient(url, token="tok-1"), "pw", "tok") as ctx:
                            with self.assertRaises(ApiError):
                                by_id[cid].fn(ctx)
                        leaked = {
                            k for k in FailAtEnableHandler.plugins if k not in ("builtin.checkin", "builtin.boost")
                        }
                        self.assertEqual(leaked, set(), f"{cid} (fail_idx={fail_idx}) leaked plugins: {leaked}")
                        self.assertFalse(FailAtEnableHandler.browser_enabled, f"{cid} leaked browser_enabled=True")

                    # T3-P05 & T4-SCEN-03: restart failure after POST /plugin-installations cleans up pid
                    for cid in ("T3-P05", "T4-SCEN-03"):
                        FailAtEnableHandler.plugins.clear()
                        with TestContext(url, ApiClient(url, token="tok-1"), "pw", "tok") as ctx:
                            with self.assertRaises(UnimplementedFeatureError):
                                by_id[cid].fn(ctx)
                        self.assertEqual(
                            set(FailAtEnableHandler.plugins.keys()),
                            set(),
                            f"{cid} leaked installed plugin on restart failure",
                        )

                    # T3-P01 & T4-SCEN-08: GET /settings/plugins 500 after POST /developer/enable restores dev_mode=False
                    for cid in ("T3-P01", "T4-SCEN-08"):
                        FailAtEnableHandler.plugins.clear()
                        FailAtEnableHandler.dev_mode = False
                        FailAtEnableHandler.dev_enable_called = False
                        FailAtEnableHandler.inject_next_dev_get_500 = False
                        fail_config["fail_dev_get_after_enable"] = True
                        with TestContext(url, ApiClient(url, token="tok-1"), "pw", "tok") as ctx:
                            with self.assertRaises(ApiError):
                                by_id[cid].fn(ctx)
                        self.assertTrue(FailAtEnableHandler.dev_enable_called, f"{cid} did not reach dev enable")
                        self.assertFalse(FailAtEnableHandler.dev_mode, f"{cid} leaked developer_mode=True")
                    fail_config["fail_dev_get_after_enable"] = False

                    # Verify _safe_restore_developer_mode polls 202 operation_id
                    FailAtEnableHandler.dev_mode = True
                    FailAtEnableHandler.dev_disable_polled = False
                    with TestContext(url, ApiClient(url, token="tok-1"), "pw", "tok") as ctx:
                        _safe_restore_developer_mode(ctx, False)
                    self.assertTrue(FailAtEnableHandler.dev_disable_polled)
                    self.assertFalse(FailAtEnableHandler.dev_mode)
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)

    def test_e2e_m3_operation_resume_headers_required(self) -> None:
        resume_calls = []

        class ResumeHandler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            state_lock = threading.Lock()
            uploads = {}
            preps = {}
            ops = {}
            plugins = {}
            configs = {}

            def log_message(self, format: str, *args: object) -> None:
                return

            def _read(self) -> bytes:
                n = int(self.headers.get("Content-Length", "0") or "0")
                return self.rfile.read(n) if n > 0 else b""

            def _send_json(self, status: int, payload: object) -> None:
                body = json.dumps(payload).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def _ok(self, data: object, status: int = 200) -> None:
                self._send_json(status, {"ok": True, "data": data})

            def do_GET(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                if path == "/api/v1/health":
                    self._ok({"status": "ok", "read_only": False})
                elif path == "/api/v1/plugins":
                    with self.state_lock:
                        self._ok(list(self.plugins.values()))
                elif path.startswith("/api/v1/plugins/") and path.endswith("/config"):
                    pid = path.split("/")[4]
                    self._ok(self.configs.setdefault(pid, {"plugin_id": pid, "revision": 1, "schema_revision": "1", "values": {}, "secrets": {}}))
                elif path.startswith("/api/v1/plugins/") and path.endswith("/audit"):
                    self._ok([{"operation_id": "op-1", "state": "succeeded"}])
                elif path.startswith("/api/v1/plugins/"):
                    pid = path.split("/")[4]
                    with self.state_lock:
                        p = self.plugins.get(pid)
                    if p is None:
                        self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "missing"}})
                    else:
                        self._ok(p)
                elif path.startswith("/api/v1/plugin-operations/"):
                    op_id = path.rsplit("/", 1)[-1]
                    with self.state_lock:
                        op = self.ops[op_id]
                    self._ok(op)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

            def do_POST(self) -> None:
                path = urllib.parse.urlsplit(self.path).path
                raw = self._read()
                req = json.loads(raw.decode("utf-8") or "{}") if raw.strip().startswith(b"{") else {}
                if path == "/api/v1/auth/login":
                    self._ok({"token": "tok-1", "user": {"id": "u1", "login": "admin", "role": "admin"}})
                elif path == "/api/v1/plugin-reauth":
                    self._ok({"reauth_token": "r-1", "purpose": req.get("purpose", "")})
                elif path == "/api/v1/plugin-trust-keys/prepare":
                    self._ok({"key_id": "0" * 16, "fingerprint": "f" * 64, "consent_token": "c-tk"})
                elif path == "/api/v1/plugin-trust-keys":
                    self._ok({"id": str(uuid.uuid4()), "key_id": "0" * 16, "status": "trusted", "revision": 1}, status=201)
                elif path == "/api/v1/plugin-uploads":
                    uid = str(uuid.uuid4())
                    ct = self.headers.get("Content-Type", "")
                    sha = ""
                    pid = ""
                    ver = "1.0.0"
                    if "boundary=" in ct:
                        b = ct.split("boundary=", 1)[1].strip().strip('"').encode("latin-1")
                        for part in raw.split(b"--" + b):
                            if b'name="file"' in part and b"\r\n\r\n" in part:
                                pay = part.split(b"\r\n\r\n", 1)[1]
                                if pay.endswith(b"\r\n"):
                                    pay = pay[:-2]
                                sha = hashlib.sha256(pay).hexdigest()
                                with zipfile.ZipFile(io.BytesIO(pay), "r") as zf:
                                    for ln in zf.read("plugin.toml").decode("utf-8").splitlines():
                                        s_ln = ln.strip()
                                        if s_ln.startswith("["):
                                            break
                                        if s_ln.startswith("id ="):
                                            pid = s_ln.split("=", 1)[1].strip().strip('"')
                                        elif s_ln.startswith("version ="):
                                            ver = s_ln.split("=", 1)[1].strip().strip('"')
                    with self.state_lock:
                        self.uploads[uid] = (sha, pid, ver)
                    self._ok({"upload_id": uid, "hash": sha, "signature_status": "verified"}, status=201)
                elif path == "/api/v1/plugin-installations/prepare":
                    op_id = str(uuid.uuid4())
                    prep_id = f"prep-{op_id}"
                    sha, pid, ver = self.uploads[req["upload_id"]]
                    with self.state_lock:
                        self.preps[prep_id] = (sha, pid, ver)
                        self.ops[op_id] = {
                            "operation_id": op_id,
                            "revision": 1,
                            "state": "succeeded",
                            "result": {
                                "preparation_id": prep_id,
                                "consent_token": "c1",
                                "package_hash": sha,
                                "grant_diff": {"added": [{"capability": "kv", "scope": "Global", "sensitive": False}], "kept": []},
                            },
                        }
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path == "/api/v1/plugin-installations":
                    idem = self.headers.get("Idempotency-Key")
                    with self.state_lock:
                        for eid, eop in self.ops.items():
                            if eop.get("idem") == idem:
                                self._ok({"operation_id": eid, "state": eop["state"]}, status=202)
                                return
                        op_id = str(uuid.uuid4())
                        sha, pid, ver = self.preps[req["preparation_id"]]
                        self.plugins[pid] = {"id": pid, "version": ver, "revision": 1, "generation": 0, "lifecycle": "installed-disabled", "package_hash": sha}
                        self.ops[op_id] = {"operation_id": op_id, "revision": 7, "state": "running", "idem": idem}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/upgrades/prepare"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    prep_id = f"prep-{op_id}"
                    sha, _, ver = self.uploads[req["upload_id"]]
                    with self.state_lock:
                        self.preps[prep_id] = (sha, pid, ver)
                        self.ops[op_id] = {
                            "operation_id": op_id,
                            "revision": 1,
                            "state": "succeeded",
                            "result": {"preparation_id": prep_id, "consent_token": "c2", "package_hash": sha, "config_diff": {"target_config_digest": "d" * 64}, "grant_diff": {"added": [], "kept": []}},
                        }
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/upgrade"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    sha, _, ver = self.preps[req["preparation_id"]]
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["version"] = ver
                        p["package_hash"] = sha
                        p["revision"] = int(p["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "revision": 9, "state": "running"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugin-operations/") and path.endswith("/resume"):
                    op_id = path.split("/")[4]
                    if_match = self.headers.get("If-Match")
                    idem = self.headers.get("Idempotency-Key")
                    resume_calls.append({"op_id": op_id, "if_match": if_match, "idempotency_key": idem})
                    with self.state_lock:
                        op = self.ops[op_id]
                        if not if_match or not idem or if_match.strip('"') != str(op["revision"]):
                            self._send_json(400, {"ok": False, "error": {"code": "invalid_headers", "message": "missing/mismatched If-Match or Idempotency-Key"}})
                            return
                        op["state"] = "succeeded"
                        op["revision"] = int(op["revision"]) + 1
                    self._ok({"operation_id": op_id, "state": "succeeded"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/enable"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["lifecycle"] = "enabled"
                        p["generation"] = int(p["generation"]) + 1
                        p["revision"] = int(p["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "revision": 1, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/disable"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        p = self.plugins[pid]
                        p["lifecycle"] = "disabled"
                        p["revision"] = int(p["revision"]) + 1
                        self.ops[op_id] = {"operation_id": op_id, "revision": 1, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                elif path.startswith("/api/v1/plugins/") and path.endswith("/uninstall"):
                    pid = path.split("/")[4]
                    op_id = str(uuid.uuid4())
                    with self.state_lock:
                        self.plugins.pop(pid, None)
                        self.ops[op_id] = {"operation_id": op_id, "revision": 1, "state": "succeeded"}
                    self._ok({"operation_id": op_id, "state": "queued"}, status=202)
                else:
                    self._send_json(404, {"ok": False, "error": {"code": "not_found", "message": "not found"}})

        class FakeSupervisor:
            def __init__(self, target_url: str, port_num: int) -> None:
                self._url = target_url
                self.port = port_num

            def kill(self) -> None:
                return None

            def start(self, extra_env: object = None, port: object = None) -> str:
                return self._url

            def restart(self, extra_env: object = None, reuse_port: bool = False) -> str:
                return self._url

        srv = _ThreadedStubHTTPServer(("127.0.0.1", 0), ResumeHandler)
        thr = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thr.start()
        try:
            port_num = srv.server_address[1]
            url = f"http://127.0.0.1:{port_num}"
            full_reg = build_default_registry()
            by_id = {c.id: c for c in full_reg.cases}
            with tempfile.TemporaryDirectory() as td:
                prev_root = os.environ.get("CRAWLER_E2E_TMP_ROOT")
                os.environ["CRAWLER_E2E_TMP_ROOT"] = td
                try:
                    for cid in ("T1-F05-04", "T3-P05", "T4-SCEN-03"):
                        ResumeHandler.plugins.clear()
                        with TestContext(url, ApiClient(url, token="tok-1"), "pw", "tok", supervisor=FakeSupervisor(url, port_num)) as ctx:  # type: ignore[arg-type]
                            by_id[cid].fn(ctx)
                    # T1-F05-04 (1) + T3-P05 (1) + T4-SCEN-03 (2: install + upgrade) = 4 /resume calls
                    self.assertEqual(len(resume_calls), 4, resume_calls)
                    self.assertEqual([c["if_match"] for c in resume_calls], ['"7"', '"7"', '"7"', '"9"'])
                    self.assertTrue(all(c["idempotency_key"] for c in resume_calls))
                finally:
                    if prev_root is None:
                        os.environ.pop("CRAWLER_E2E_TMP_ROOT", None)
                    else:
                        os.environ["CRAWLER_E2E_TMP_ROOT"] = prev_root
        finally:
            srv.shutdown()
            srv.server_close()
            thr.join(timeout=2.0)


if __name__ == "__main__":
    unittest.main()
