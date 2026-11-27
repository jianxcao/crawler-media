"""Deterministic fixture builders for E2E tests (Python 3.9+ stdlib only).

Includes:
- Pure-Python RFC 8032 Ed25519 + Minisign keypair, detached signer, and verifier
- Pure-Python canonical bencode encoder/decoder + .torrent builder & negative fixtures
- Deterministic .cmplugin ZIP archive builder using real wasm32-wasip2 components
- Negative archive, manifest, JSON Schema, and UI asset-manifest hazard builders
"""
from __future__ import annotations

import base64
from dataclasses import dataclass
import hashlib
import io
import json
import os
from pathlib import Path
import struct
from typing import Any, Dict, List, Optional, Sequence, Tuple, Union
import urllib.parse
import warnings
import zipfile

from .instance import find_repo_root


# ---------------------------------------------------------------------------
# Pure-Python RFC 8032 Ed25519
# ---------------------------------------------------------------------------

_P = 2 ** 255 - 19
_L = 2 ** 252 + 27742317777372353535851937790883648493
_D = (-121665 * pow(121666, _P - 2, _P)) % _P
_I = pow(2, (_P - 1) // 4, _P)


def _recover_x(y: int, sign: int) -> Optional[int]:
    if y >= _P:
        return None
    x2 = (y * y - 1) * pow((_D * y * y + 1) % _P, _P - 2, _P) % _P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P != 0:
        x = (x * _I) % _P
    if (x * x - x2) % _P != 0:
        return None
    if (x & 1) != sign:
        x = _P - x
    return x


_BY = (4 * pow(5, _P - 2, _P)) % _P
_BX = _recover_x(_BY, 0)
assert _BX is not None
_B_POINT = (_BX, _BY, 1, (_BX * _BY) % _P)
_ZERO_POINT = (0, 1, 1, 0)


def _point_add(p1: Tuple[int, int, int, int], p2: Tuple[int, int, int, int]) -> Tuple[int, int, int, int]:
    x1, y1, z1, t1 = p1
    x2, y2, z2, t2 = p2
    a = ((y1 - x1) * (y2 - x2)) % _P
    b = ((y1 + x1) * (y2 + x2)) % _P
    c = (2 * _D * t1 * t2) % _P
    d = (2 * z1 * z2) % _P
    e = (b - a) % _P
    f = (d - c) % _P
    g = (d + c) % _P
    h = (b + a) % _P
    return (
        (e * f) % _P,
        (g * h) % _P,
        (f * g) % _P,
        (e * h) % _P,
    )


def _point_double(p1: Tuple[int, int, int, int]) -> Tuple[int, int, int, int]:
    x1, y1, z1, _ = p1
    a = (x1 * x1) % _P
    b = (y1 * y1) % _P
    c = (2 * z1 * z1) % _P
    d = (-a) % _P
    e = (((x1 + y1) * (x1 + y1)) - a - b) % _P
    g = (d + b) % _P
    f = (g - c) % _P
    h = (d - b) % _P
    return (
        (e * f) % _P,
        (g * h) % _P,
        (f * g) % _P,
        (e * h) % _P,
    )


def _scalar_mult(pt: Tuple[int, int, int, int], scalar: int) -> Tuple[int, int, int, int]:
    acc = _ZERO_POINT
    cur = pt
    k = scalar
    while k > 0:
        if k & 1:
            acc = _point_add(acc, cur)
        cur = _point_double(cur)
        k >>= 1
    return acc


def _encode_point(pt: Tuple[int, int, int, int]) -> bytes:
    x, y, z, _ = pt
    z_inv = pow(z, _P - 2, _P)
    rx = (x * z_inv) % _P
    ry = (y * z_inv) % _P
    val = ry | ((rx & 1) << 255)
    return val.to_bytes(32, "little")


def _decode_point(raw: bytes) -> Optional[Tuple[int, int, int, int]]:
    if len(raw) != 32:
        return None
    val = int.from_bytes(raw, "little")
    sign = (val >> 255) & 1
    y = val & ((1 << 255) - 1)
    x = _recover_x(y, sign)
    if x is None:
        return None
    return (x, y, 1, (x * y) % _P)


def _points_equal(p1: Tuple[int, int, int, int], p2: Tuple[int, int, int, int]) -> bool:
    x1, y1, z1, _ = p1
    x2, y2, z2, _ = p2
    return (x1 * z2 - x2 * z1) % _P == 0 and (y1 * z2 - y2 * z1) % _P == 0


def _clamp_scalar(digest_first_32: bytes) -> int:
    arr = bytearray(digest_first_32)
    arr[0] &= 248
    arr[31] &= 127
    arr[31] |= 64
    return int.from_bytes(arr, "little")


def ed25519_public_key(secret_seed: bytes) -> bytes:
    if len(secret_seed) != 32:
        raise ValueError("Ed25519 secret seed must be exactly 32 bytes")
    h = hashlib.sha512(secret_seed).digest()
    a = _clamp_scalar(h[:32])
    return _encode_point(_scalar_mult(_B_POINT, a))


def ed25519_sign(secret_seed: bytes, message: bytes) -> bytes:
    if len(secret_seed) != 32:
        raise ValueError("Ed25519 secret seed must be exactly 32 bytes")
    h = hashlib.sha512(secret_seed).digest()
    a = _clamp_scalar(h[:32])
    prefix = h[32:]
    pub = _encode_point(_scalar_mult(_B_POINT, a))

    r = int.from_bytes(hashlib.sha512(prefix + message).digest(), "little") % _L
    r_enc = _encode_point(_scalar_mult(_B_POINT, r))
    k = int.from_bytes(hashlib.sha512(r_enc + pub + message).digest(), "little") % _L
    s = (r + k * a) % _L
    return r_enc + s.to_bytes(32, "little")


def ed25519_verify(public_key: bytes, message: bytes, signature: bytes) -> bool:
    if len(public_key) != 32 or len(signature) != 64:
        return False
    a_pt = _decode_point(public_key)
    r_pt = _decode_point(signature[:32])
    if a_pt is None or r_pt is None:
        return False
    if _points_equal(_scalar_mult(a_pt, 8), _ZERO_POINT) or _points_equal(
        _scalar_mult(r_pt, 8), _ZERO_POINT
    ):
        return False
    s = int.from_bytes(signature[32:], "little")
    if s >= _L:
        return False
    k = int.from_bytes(hashlib.sha512(signature[:32] + public_key + message).digest(), "little") % _L
    lhs = _scalar_mult(_B_POINT, s)
    rhs = _point_add(r_pt, _scalar_mult(a_pt, k))
    # Cofactor check for strict RFC 8032 equivalence on torsion components
    return _points_equal(_scalar_mult(lhs, 8), _scalar_mult(rhs, 8))


# ---------------------------------------------------------------------------
# Minisign Keypair, Signer & Verifier
# ---------------------------------------------------------------------------


class MinisignError(ValueError):
    """Raised when minisign parsing or verification fails."""


def _format_minisign_key_id(key_id: bytes) -> str:
    return f"{int.from_bytes(key_id, 'little'):016X}"


@dataclass(frozen=True)
class MinisignKeypair:
    key_id: bytes
    public_key: bytes
    secret_seed: bytes
    public_key_b64: str
    public_key_text: str
    fingerprint: str

    @property
    def key_id_hex(self) -> str:
        return _format_minisign_key_id(self.key_id)


@dataclass(frozen=True)
class MinisignVerification:
    valid: bool
    algorithm: str
    key_id_hex: str
    trusted_comment: str
    reason: Optional[str] = None


def generate_minisign_keypair(
    seed: Optional[bytes] = None,
    key_id: Optional[bytes] = None,
) -> MinisignKeypair:
    """Generate a Minisign-compatible Ed25519 keypair."""
    if seed is None:
        seed = os.urandom(32)
    elif len(seed) != 32:
        raise ValueError("seed must be 32 bytes")

    if key_id is None:
        key_id = hashlib.blake2b(seed, digest_size=8).digest()
    elif len(key_id) != 8:
        raise ValueError("key_id must be 8 bytes")

    pubkey = ed25519_public_key(seed)
    raw_pub = b"Ed" + key_id + pubkey
    pub_b64 = base64.b64encode(raw_pub).decode("ascii")
    pub_text = (
        f"untrusted comment: minisign public key {_format_minisign_key_id(key_id)}\n"
        f"{pub_b64}\n"
    )
    fingerprint = hashlib.sha256(raw_pub).hexdigest()
    return MinisignKeypair(
        key_id=key_id,
        public_key=pubkey,
        secret_seed=seed,
        public_key_b64=pub_b64,
        public_key_text=pub_text,
        fingerprint=fingerprint,
    )


def sign_minisign(
    data: bytes,
    keypair: MinisignKeypair,
    hashed: bool = True,
    trusted_comment: Optional[str] = None,
    untrusted_comment: Optional[str] = None,
    tamper_sig: bool = False,
    tamper_global: bool = False,
    tamper_comment: bool = False,
    wrong_key_id: Optional[bytes] = None,
) -> str:
    """Create a 4-line detached .minisig signature over `data`.

    Supports prehashed `ED` (BLAKE2b-512) and legacy `Ed` algorithms, plus
    deliberate corruption toggles for negative E2E cases.
    """
    sig_alg = b"ED" if hashed else b"Ed"
    payload = hashlib.blake2b(data, digest_size=64).digest() if hashed else data
    sig64 = bytearray(ed25519_sign(keypair.secret_seed, payload))
    if tamper_sig:
        sig64[0] ^= 0xFF

    if trusted_comment is None:
        suffix = "\thashed" if hashed else ""
        trusted_comment = f"timestamp:1700000000\tfile:plugin.cmplugin{suffix}"

    global_sig = bytearray(
        ed25519_sign(keypair.secret_seed, bytes(sig64) + trusted_comment.encode("utf-8"))
    )
    if tamper_global:
        global_sig[0] ^= 0xFF
    if tamper_comment:
        trusted_comment = trusted_comment + "\ttampered=1"

    eff_key_id = wrong_key_id if wrong_key_id is not None else keypair.key_id
    if len(eff_key_id) != 8:
        raise ValueError("wrong_key_id must be 8 bytes")

    line1 = f"untrusted comment: {untrusted_comment or 'signature from minisign secret key'}"
    line2 = base64.b64encode(sig_alg + eff_key_id + bytes(sig64)).decode("ascii")
    line3 = f"trusted comment: {trusted_comment}"
    line4 = base64.b64encode(bytes(global_sig)).decode("ascii")
    return "\n".join([line1, line2, line3, line4]) + "\n"


def sign_cmplugin_sha256(
    cmplugin_bytes: bytes,
    keypair: MinisignKeypair,
    hashed: bool = True,
    **kwargs: Any,
) -> str:
    """Sign the 32-byte SHA-256 digest of a .cmplugin package per authoring.md §8.3."""
    digest = hashlib.sha256(cmplugin_bytes).digest()
    return sign_minisign(digest, keypair, hashed=hashed, **kwargs)


def parse_minisign_public_key(public_key_text: str) -> Tuple[bytes, bytes]:
    lines = [ln.strip() for ln in public_key_text.strip().splitlines() if ln.strip()]
    if not lines:
        raise MinisignError("Empty minisign public key")
    b64_line = lines[1] if len(lines) >= 2 and lines[0].startswith("untrusted comment:") else lines[0]
    try:
        raw = base64.b64decode(b64_line, validate=True)
    except Exception as exc:
        raise MinisignError(f"Invalid base64 public key: {exc}") from exc
    if len(raw) != 42 or raw[:2] != b"Ed":
        raise MinisignError("Invalid minisign public key header or length")
    return raw[2:10], raw[10:42]


def verify_minisign(
    data: bytes,
    sig_text: str,
    public_key_text: str,
) -> MinisignVerification:
    """Verify a detached 4-line minisign signature against `data` and `public_key_text`."""
    try:
        pub_key_id, pubkey = parse_minisign_public_key(public_key_text)
    except MinisignError as exc:
        return MinisignVerification(False, "", "", "", str(exc))

    lines = sig_text.splitlines()
    if len(lines) < 4:
        return MinisignVerification(False, "", "", "", "Signature must have at least 4 lines")
    if not lines[0].startswith("untrusted comment:"):
        return MinisignVerification(False, "", "", "", "Missing untrusted comment header")
    if not lines[2].startswith("trusted comment: "):
        return MinisignVerification(False, "", "", "", "Missing trusted comment header")

    trusted_comment = lines[2][len("trusted comment: ") :]
    try:
        sig_blob = base64.b64decode(lines[1].strip(), validate=True)
        global_sig = base64.b64decode(lines[3].strip(), validate=True)
    except Exception as exc:
        return MinisignVerification(False, "", "", trusted_comment, f"Invalid base64 signature: {exc}")

    if len(sig_blob) != 74 or len(global_sig) != 64:
        return MinisignVerification(False, "", "", trusted_comment, "Invalid signature binary length")

    alg = sig_blob[:2]
    if alg not in (b"ED", b"Ed"):
        return MinisignVerification(False, repr(alg), "", trusted_comment, "Unsupported signature algorithm")

    sig_key_id = sig_blob[2:10]
    sig_key_id_hex = _format_minisign_key_id(sig_key_id)
    sig64 = sig_blob[10:74]
    if sig_key_id != pub_key_id:
        return MinisignVerification(
            False,
            alg.decode("ascii"),
            sig_key_id_hex,
            trusted_comment,
            "Key ID mismatch",
        )

    payload = hashlib.blake2b(data, digest_size=64).digest() if alg == b"ED" else data
    if not ed25519_verify(pubkey, payload, sig64):
        return MinisignVerification(
            False,
            alg.decode("ascii"),
            sig_key_id_hex,
            trusted_comment,
            "Data signature verification failed",
        )

    global_msg = sig64 + trusted_comment.encode("utf-8")
    if not ed25519_verify(pubkey, global_msg, global_sig):
        return MinisignVerification(
            False,
            alg.decode("ascii"),
            sig_key_id_hex,
            trusted_comment,
            "Global signature over trusted comment failed",
        )

    return MinisignVerification(
        True,
        alg.decode("ascii"),
        sig_key_id_hex,
        trusted_comment,
    )


# ---------------------------------------------------------------------------
# Pure-Python Bencode & .torrent Builders
# ---------------------------------------------------------------------------


class BencodeError(ValueError):
    """Raised when encoding or decoding invalid bencode."""


def bencode_encode(value: Any) -> bytes:
    if isinstance(value, bool):
        raise BencodeError("Bencode does not support Python bool directly")
    if isinstance(value, int):
        return f"i{value}e".encode("ascii")
    if isinstance(value, bytes):
        return f"{len(value)}:".encode("ascii") + value
    if isinstance(value, str):
        raw = value.encode("utf-8")
        return f"{len(raw)}:".encode("ascii") + raw
    if isinstance(value, (list, tuple)):
        return b"l" + b"".join(bencode_encode(item) for item in value) + b"e"
    if isinstance(value, dict):
        items: List[Tuple[bytes, Any]] = []
        for k, v in value.items():
            if isinstance(k, bytes):
                kb = k
            elif isinstance(k, str):
                kb = k.encode("utf-8")
            else:
                raise BencodeError(f"Dict keys must be str or bytes, got {type(k).__name__}")
            items.append((kb, v))
        items.sort(key=lambda pair: pair[0])
        out = [b"d"]
        for kb, v in items:
            out.append(f"{len(kb)}:".encode("ascii") + kb)
            out.append(bencode_encode(v))
        out.append(b"e")
        return b"".join(out)
    raise BencodeError(f"Unsupported type for bencode: {type(value).__name__}")


def bencode_decode(data: bytes, max_depth: int = 64) -> Any:
    def _decode_at(pos: int, depth: int) -> Tuple[Any, int]:
        if depth > max_depth:
            raise BencodeError(f"Bencode nesting exceeds max_depth={max_depth}")
        if pos >= len(data):
            raise BencodeError("Unexpected EOF in bencode")
        ch = data[pos : pos + 1]
        if ch == b"i":
            end = data.find(b"e", pos + 1)
            if end == -1:
                raise BencodeError("Unterminated bencode integer")
            num_bytes = data[pos + 1 : end]
            if not num_bytes:
                raise BencodeError("Empty bencode integer")
            digits = num_bytes[1:] if num_bytes.startswith(b"-") else num_bytes
            if not digits or not all(48 <= b <= 57 for b in digits):
                raise BencodeError(f"Invalid bencode integer: {num_bytes!r}")
            if num_bytes == b"-0" or (len(digits) > 1 and digits.startswith(b"0")):
                raise BencodeError(f"Non-canonical bencode integer: {num_bytes!r}")
            val = int(num_bytes.decode("ascii"))
            return val, end + 1
        if ch == b"l":
            cur = pos + 1
            items = []
            while cur < len(data) and data[cur : cur + 1] != b"e":
                item, cur = _decode_at(cur, depth + 1)
                items.append(item)
            if cur >= len(data):
                raise BencodeError("Unterminated bencode list")
            return items, cur + 1
        if ch == b"d":
            cur = pos + 1
            out: Dict[bytes, Any] = {}
            prev_key: Optional[bytes] = None
            while cur < len(data) and data[cur : cur + 1] != b"e":
                key, cur = _decode_at(cur, depth + 1)
                if not isinstance(key, bytes):
                    raise BencodeError("Bencode dict key must be byte string")
                if prev_key is not None and key <= prev_key:
                    raise BencodeError("Bencode dict keys must be strictly sorted and unique")
                prev_key = key
                val, cur = _decode_at(cur, depth + 1)
                out[key] = val
            if cur >= len(data):
                raise BencodeError("Unterminated bencode dict")
            return out, cur + 1
        if b"0" <= ch <= b"9":
            colon = data.find(b":", pos)
            if colon == -1:
                raise BencodeError("Missing colon in bencode byte string")
            len_bytes = data[pos:colon]
            if len(len_bytes) > 1 and len_bytes.startswith(b"0"):
                raise BencodeError("Non-canonical byte string length")
            if not len_bytes or not all(48 <= b <= 57 for b in len_bytes):
                raise BencodeError(f"Invalid bencode byte string length: {len_bytes!r}")
            try:
                length = int(len_bytes.decode("ascii"))
            except (UnicodeDecodeError, ValueError) as exc:
                raise BencodeError(f"Invalid bencode byte string length: {len_bytes!r}") from exc
            start = colon + 1
            end = start + length
            if end > len(data):
                raise BencodeError("Truncated bencode byte string")
            return data[start:end], end
        raise BencodeError(f"Unexpected token {ch!r} at offset {pos}")

    val, next_pos = _decode_at(0, 0)
    if next_pos != len(data):
        raise BencodeError(f"Trailing bytes at offset {next_pos}")
    return val


@dataclass(frozen=True)
class TorrentFixture:
    bytes: bytes
    info_hash: str
    verified_total_bytes: int
    bytes_digest: str
    name: str
    is_multi_file: bool


def build_torrent_bytes(
    name: str = "e2e-release",
    length: Optional[int] = None,
    files: Optional[List[Tuple[List[str], int]]] = None,
    piece_length: int = 16384,
    announce: str = "http://127.0.0.1:6969/announce",
    comment: Optional[str] = None,
) -> TorrentFixture:
    """Build a valid single-file or multi-file .torrent payload."""
    if files is not None and length is not None:
        raise ValueError("Specify either length (single-file) or files (multi-file), not both")
    if files is None and length is None:
        length = 1048576

    info: Dict[str, Any] = {
        "name": name,
        "piece length": piece_length,
        "pieces": hashlib.sha1(name.encode("utf-8")).digest(),
    }
    if files is not None:
        info["files"] = [
            {"length": flen, "path": list(fpath)} for fpath, flen in files
        ]
        total_bytes = sum(flen for _, flen in files)
        is_multi = True
    else:
        assert length is not None
        info["length"] = length
        total_bytes = length
        is_multi = False

    root: Dict[str, Any] = {
        "announce": announce,
        "info": info,
    }
    if comment is not None:
        root["comment"] = comment

    info_bencoded = bencode_encode(info)
    info_hash = hashlib.sha1(info_bencoded).hexdigest()
    torrent_bytes = bencode_encode(root)
    return TorrentFixture(
        bytes=torrent_bytes,
        info_hash=info_hash,
        verified_total_bytes=total_bytes,
        bytes_digest=hashlib.sha256(torrent_bytes).hexdigest(),
        name=name,
        is_multi_file=is_multi,
    )


def build_dishonest_torrent(
    name: str = "dishonest-release",
    actual_bytes: int = 1024 ** 4,
) -> TorrentFixture:
    """Build a valid .torrent whose real metainfo size (e.g. 1 TiB) exceeds indexer lies."""
    return build_torrent_bytes(name=name, length=actual_bytes)


def build_overflow_torrent(name: str = "overflow-release") -> TorrentFixture:
    """Build a multi-file .torrent whose summed file lengths overflow signed i64 (2**63)."""
    half = 2 ** 62
    return build_torrent_bytes(
        name=name,
        files=[(["part1.bin"], half), (["part2.bin"], half)],
    )


def build_negative_length_torrent(name: str = "negative-release") -> bytes:
    """Build a .torrent with negative file length."""
    return build_torrent_bytes(name=name, length=-1).bytes


def build_malformed_torrent(kind: str = "truncated") -> bytes:
    if kind == "truncated":
        return b"d8:announce10:http://127"
    if kind == "missing_info":
        return bencode_encode({"announce": "http://127.0.0.1:6969/announce"})
    if kind == "deep_nesting":
        return (b"l" * 80) + (b"e" * 80)
    return b"not-bencode-at-all"


def build_missing_file_length_torrent(name: str = "missing-length-release") -> bytes:
    """Build a multi-file .torrent whose files[] entry omits required `length`."""
    info = {
        "name": name,
        "piece length": 16384,
        "pieces": hashlib.sha1(name.encode("utf-8")).digest(),
        "files": [{"path": ["part1.mkv"]}],
    }
    return bencode_encode({"announce": "http://127.0.0.1:6969/announce", "info": info})


def build_v2_only_torrent(name: str = "v2-only-release") -> bytes:
    """Build a BitTorrent v2-only metainfo without v1 `length` or `files`."""
    info = {
        "name": name,
        "meta version": 2,
        "piece length": 16384,
        "file tree": {
            "movie.mkv": {
                "": {
                    "length": 4096,
                    "pieces root": b"\x00" * 32,
                }
            }
        },
    }
    return bencode_encode({"announce": "http://127.0.0.1:6969/announce", "info": info})


def build_oversized_torrent_bytes(
    size_bytes: int = 10 * 1024 * 1024 + 1024,
    name: str = "oversized-release",
) -> bytes:
    """Build a valid bencode .torrent whose serialized payload exceeds `size_bytes`."""
    base = build_torrent_bytes(name=name, length=1048576, comment="").bytes
    pad_len = max(1, size_bytes - len(base) + 64)
    return build_torrent_bytes(name=name, length=1048576, comment=("x" * pad_len)).bytes


def build_magnet_uri(
    info_hash: str,
    dn: str = "demo",
    xl: Optional[int] = None,
) -> str:
    """Build a magnet URI (optionally omitting `xl` for unknown-size tests)."""
    uri = f"magnet:?xt=urn:btih:{info_hash}&dn={urllib.parse.quote(dn)}"
    if xl is not None:
        uri += f"&xl={int(xl)}"
    return uri


# ---------------------------------------------------------------------------
# .cmplugin Package Builders & Negative Fixtures
# ---------------------------------------------------------------------------


_FIXTURE_WASM_MAP = {
    "policy": "policy.wasm",
    "hook": "policy.wasm",
    "task": "task_fixture.wasm",
    "endpoint": "endpoint_fixture.wasm",
    "indexer": "indexer_fixture.wasm",
    "maintenance": "maintenance_fixture.wasm",
}

_DEFAULT_CONFIG_SCHEMA: Dict[str, Any] = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": False,
    "properties": {
        "dry_run": {"type": "boolean", "default": True},
        "threshold": {"type": "integer", "minimum": 0, "maximum": 1000, "default": 10},
    },
}


def build_secret_config_schema() -> Dict[str, Any]:
    """Return a bounded config JSON Schema containing public and secret fields."""
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": False,
        "required": ["profile_label"],
        "properties": {
            "profile_label": {"type": "string", "minLength": 1, "default": "demo"},
            "dry_run": {"type": "boolean", "default": True},
            "threshold": {"type": "integer", "minimum": 0, "maximum": 1000, "default": 10},
            "api_token": {
                "type": "string",
                "writeOnly": True,
                "format": "password",
                "x-ui": {"widget": "password"},
            },
        },
    }


@dataclass(frozen=True)
class CmpluginPackage:
    bytes: bytes
    sha256_hex: str
    sha256_bytes: bytes
    plugin_id: str
    version: str
    manifest_toml: str
    entries: Tuple[str, ...]


def load_fixture_wasm(kind: str = "policy") -> bytes:
    """Read one of the prebuilt wasm32-wasip2 component fixtures from crates/plugin-host."""
    filename = _FIXTURE_WASM_MAP.get(kind, kind)
    fixtures_dir = (
        find_repo_root()
        / "crates"
        / "plugin-host"
        / "tests"
        / "fixtures"
    )
    wasm_path = fixtures_dir / filename
    if not wasm_path.is_file():
        if kind in ("indexer", "maintenance"):
            fallback = fixtures_dir / "policy.wasm"
            if fallback.is_file():
                return fallback.read_bytes()
        raise FileNotFoundError(f"Prebuilt WASM fixture not found: {wasm_path}")
    return wasm_path.read_bytes()


def _toml_str(val: str) -> str:
    return json.dumps(val, ensure_ascii=False)


def _toml_str_list(items: Sequence[str]) -> str:
    return "[" + ", ".join(_toml_str(x) for x in items) + "]"


def build_plugin_toml(
    plugin_id: str = "org.example.e2e-policy",
    name: str = "E2E Policy Plugin",
    version: str = "1.0.0",
    api: str = "1.0.0",
    license_name: str = "MIT",
    authors: Sequence[str] = ("E2E Suite",),
    description: str = "Deterministic E2E fixture plugin",
    config_schema: str = "schemas/config.schema.json",
    state_schema: int = 1,
    components: Optional[List[Dict[str, Any]]] = None,
    sites: Optional[Sequence[str]] = None,
    ui: Optional[Dict[str, Any]] = None,
    limits: Optional[Dict[str, int]] = None,
    credential_binding: Optional[str] = None,
    http_rules: Optional[List[Dict[str, Any]]] = None,
    extra_top_level: Optional[Dict[str, Any]] = None,
) -> str:
    lines: List[str] = [
        "manifest_version = 1",
        f"id = {_toml_str(plugin_id)}",
        f"name = {_toml_str(name)}",
        f"version = {_toml_str(version)}",
        f"api = {_toml_str(api)}",
        f"license = {_toml_str(license_name)}",
        f"authors = {_toml_str_list(authors)}",
        f"description = {_toml_str(description)}",
        f"config_schema = {_toml_str(config_schema)}",
        f"state_schema = {state_schema}",
    ]
    if credential_binding is not None:
        lines.append(f"credential_binding = {_toml_str(credential_binding)}")
    if sites is not None:
        lines.append(f"sites = {_toml_str_list(sites)}")
    if extra_top_level:
        for k, v in extra_top_level.items():
            if isinstance(v, bool):
                lines.append(f"{k} = {'true' if v else 'false'}")
            elif isinstance(v, int):
                lines.append(f"{k} = {v}")
            else:
                lines.append(f"{k} = {_toml_str(str(v))}")

    for rule in http_rules or []:
        lines.append("")
        lines.append("[[http]]")
        lines.append(f"origin = {_toml_str(rule['origin'])}")
        lines.append(f"path_prefix = {_toml_str(rule.get('path_prefix', '/'))}")
        lines.append(f"methods = {_toml_str_list(rule.get('methods', ['GET']))}")

    for comp in components or []:
        lines.append("")
        lines.append("[[components]]")
        lines.append(f"id = {_toml_str(comp['id'])}")
        lines.append(f"entry = {_toml_str(comp['entry'])}")
        lines.append(f"world = {_toml_str(comp['world'])}")
        lines.append(f"permissions = {_toml_str_list(comp.get('permissions', []))}")
        for hk in comp.get("hooks", []):
            lines.append("")
            lines.append("[[components.hooks]]")
            lines.append(f"id = {_toml_str(hk['id'])}")
            lines.append(f"priority = {hk.get('priority', 400)}")
        for tsk in comp.get("tasks", []):
            lines.append("")
            lines.append("[[components.tasks]]")
            lines.append(f"id = {_toml_str(tsk['id'])}")
            lines.append(f"input_schema = {_toml_str(tsk.get('input_schema', config_schema))}")
            secs = tsk.get("seconds", 1800)
            lines.append(f'schedule = {{ kind = "interval", seconds = {secs} }}')
            lines.append(f"concurrency_key = {_toml_str(tsk.get('concurrency_key', tsk['id']))}")
        for ep in comp.get("endpoints", []):
            lines.append("")
            lines.append("[[components.endpoints]]")
            lines.append(f"id = {_toml_str(ep['id'])}")
            lines.append(f"method = {_toml_str(ep.get('method', 'POST'))}")
            lines.append(f"path = {_toml_str(ep.get('path', '/run'))}")
            lines.append(f"auth = {_toml_str(ep.get('auth', 'admin'))}")
            lines.append(f"input_schema = {_toml_str(ep.get('input_schema', config_schema))}")
            lines.append(f"capabilities = {_toml_str_list(ep.get('capabilities', []))}")
        for idx in comp.get("indexers", []):
            lines.append("")
            lines.append("[[components.indexers]]")
            lines.append(f"id = {_toml_str(idx['id'])}")
            lines.append(f"name = {_toml_str(idx.get('name', 'E2E Indexer'))}")
            lines.append(f"kind = {_toml_str(idx.get('kind', 'nexusphp'))}")
            lines.append(
                f"capabilities = {_toml_str_list(idx.get('capabilities', ['search', 'resolve-download']))}"
            )
            lines.append(f"site_schema = {_toml_str(idx.get('site_schema', config_schema))}")
        for maint in comp.get("maintenance", []):
            lines.append("")
            lines.append("[[components.maintenance]]")
            lines.append(f"id = {_toml_str(maint['id'])}")
            lines.append(
                f"actions = {_toml_str_list(maint.get('actions', ['verify-session', 'check-in']))}"
            )

    eff_limits = limits if limits is not None else {
        "memory_mib": 64,
        "fuel_limit": 10000000,
        "io_ms": 1500,
        "wall_ms": 2000,
        "http_calls": 8,
        "response_bytes": 5242880,
        "kv_bytes": 1048576,
    }
    if eff_limits:
        lines.append("")
        lines.append("[limits]")
        for k, v in eff_limits.items():
            lines.append(f"{k} = {v}")

    if ui is not None:
        lines.append("")
        lines.append("[ui]")
        lines.append(f"kind = {_toml_str(ui.get('kind', 'federation-react'))}")
        lines.append(f"contract = {_toml_str(ui.get('contract', '1.0.0'))}")
        lines.append(f"remote_name = {_toml_str(ui['remote_name'])}")
        lines.append(f"entry = {_toml_str(ui.get('entry', 'ui/assets/remoteEntry.js'))}")
        lines.append(
            f"asset_manifest = {_toml_str(ui.get('asset_manifest', 'ui/asset-manifest.json'))}"
        )
        lines.append(f"react = {_toml_str(ui.get('react', '19.3.0'))}")
        lines.append(f"react_dom = {_toml_str(ui.get('react_dom', '19.3.0'))}")
        lines.append(f"capabilities = {_toml_str_list(ui.get('capabilities', []))}")
        for surf in ui.get("surfaces", []):
            lines.append("")
            lines.append("[[ui.surfaces]]")
            lines.append(f"id = {_toml_str(surf['id'])}")
            lines.append(f"kind = {_toml_str(surf['kind'])}")
            lines.append(f"expose = {_toml_str(surf['expose'])}")
            lines.append(f"title = {_toml_str(surf['title'])}")
            if surf.get("icon"):
                lines.append(f"icon = {_toml_str(surf['icon'])}")

    return "\n".join(lines) + "\n"


def build_ui_asset_bundle(
    remote_name: str = "e2eUiPlugin",
    extra_assets: Optional[Dict[str, Tuple[bytes, str]]] = None,
    manifest_version: int = 1,
    extra_manifest_entries: Optional[List[Dict[str, Any]]] = None,
    omit_from_manifest: Sequence[str] = (),
) -> Dict[str, bytes]:
    """Create deterministic UI federation assets + asset-manifest.json."""
    assets: Dict[str, Tuple[bytes, str]] = {
        "ui/assets/remoteEntry.js": (
            f"export const remoteName = {json.dumps(remote_name)};\n".encode("utf-8"),
            "application/javascript",
        ),
        "ui/assets/style.css": (
            b".cm-plugin-root { color: #e2e8f0; }\n",
            "text/css",
        ),
    }
    if extra_assets:
        assets.update(extra_assets)

    omit_set = set(omit_from_manifest)
    manifest_files = []
    out: Dict[str, bytes] = {}
    for rel_path in sorted(assets.keys()):
        data, content_type = assets[rel_path]
        out[rel_path] = data
        if rel_path in omit_set:
            continue
        manifest_files.append(
            {
                "path": rel_path,
                "sha256": hashlib.sha256(data).hexdigest(),
                "size": len(data),
                "content_type": content_type,
            }
        )
    if extra_manifest_entries:
        manifest_files.extend(extra_manifest_entries)
    out["ui/asset-manifest.json"] = (
        json.dumps({"version": manifest_version, "files": manifest_files}, indent=2, sort_keys=True)
        + "\n"
    ).encode("utf-8")
    return out


def _write_deterministic_zip(
    entries: Sequence[Tuple[str, bytes, Optional[int], Optional[int]]],
    allow_duplicates: bool = False,
    extra_field_overrides: Optional[Dict[str, bytes]] = None,
) -> bytes:
    """Write a deterministic ZIP archive.

    Each entry is (arcname, payload, external_attr, compress_type).
    """
    buf = io.BytesIO()
    extras = extra_field_overrides or {}
    with warnings.catch_warnings():
        if allow_duplicates:
            warnings.simplefilter("ignore", UserWarning)
        with zipfile.ZipFile(buf, mode="w", compression=zipfile.ZIP_DEFLATED) as zf:
            for arcname, payload, ext_attr, comp_type in entries:
                zi = zipfile.ZipInfo(filename=arcname, date_time=(2026, 1, 1, 0, 0, 0))
                zi.compress_type = (
                    comp_type if comp_type is not None else zipfile.ZIP_DEFLATED
                )
                zi.create_system = 3  # Unix
                zi.external_attr = ext_attr if ext_attr is not None else (0o100644 << 16)
                if arcname in extras:
                    zi.extra = extras[arcname]
                zf.writestr(zi, payload)
    return buf.getvalue()


def build_cmplugin(
    plugin_id: str = "org.example.e2e-policy",
    version: str = "1.0.0",
    kind: str = "hook",
    state_schema: int = 1,
    config_schema_obj: Optional[Dict[str, Any]] = None,
    schema_override: Optional[Dict[str, Any]] = None,
    include_ui: bool = False,
    include_site_yaml: bool = False,
    extra_files: Optional[Dict[str, bytes]] = None,
    manifest_override: Optional[str] = None,
    manifest_overrides: Optional[Dict[str, Any]] = None,
    hooks: Optional[List[Dict[str, Any]]] = None,
    hook_permissions: Optional[List[str]] = None,
    external_attr_overrides: Optional[Dict[str, int]] = None,
    compress_overrides: Optional[Dict[str, int]] = None,
    site_id: str = "e2e-demo",
    site_framework: str = "nexusphp",
    ui_overrides: Optional[Dict[str, Any]] = None,
    extra_ui_assets: Optional[Dict[str, Tuple[bytes, str]]] = None,
    credential_binding: Optional[str] = None,
    http_rules: Optional[List[Dict[str, Any]]] = None,
    authors: Optional[Sequence[str]] = None,
    limits: Optional[Dict[str, int]] = None,
    extra_components: Optional[List[Dict[str, Any]]] = None,
) -> CmpluginPackage:
    """Build a deterministic valid .cmplugin archive.

    `kind`: "hook" | "task" | "endpoint" | "indexer" | "maintenance" | "yaml-only" | "ui-only"
    """
    files: Dict[str, bytes] = {}
    eff_schema = (
        schema_override
        if schema_override is not None
        else (config_schema_obj if config_schema_obj is not None else _DEFAULT_CONFIG_SCHEMA)
    )
    files["schemas/config.schema.json"] = (
        json.dumps(eff_schema, indent=2, sort_keys=True) + "\n"
    ).encode("utf-8")
    files["README.md"] = f"# {plugin_id}\n".encode("utf-8")
    files["LICENSE"] = b"MIT\n"

    components: List[Dict[str, Any]] = []
    sites: Optional[List[str]] = None
    ui_cfg: Optional[Dict[str, Any]] = None
    eff_cred_binding = credential_binding

    if kind == "hook":
        files["components/policy.wasm"] = load_fixture_wasm("policy")
        eff_hooks = (
            list(hooks)
            if hooks is not None
            else [{"id": "search.after", "priority": 400}]
        )
        eff_perms = (
            list(hook_permissions)
            if hook_permissions is not None
            else [f"hooks:{h['id']}" for h in eff_hooks] + ["kv"]
        )
        components.append(
            {
                "id": "policy",
                "entry": "components/policy.wasm",
                "world": "crawler:plugin/hook-plugin@1.0.0",
                "permissions": eff_perms,
                "hooks": eff_hooks,
            }
        )
    elif kind == "task":
        files["components/tasks.wasm"] = load_fixture_wasm("task")
        components.append(
            {
                "id": "tasks",
                "entry": "components/tasks.wasm",
                "world": "crawler:plugin/task-plugin@1.0.0",
                "permissions": ["kv", "read:sites"],
                "tasks": [
                    {
                        "id": "sync-cycle",
                        "input_schema": "schemas/config.schema.json",
                        "seconds": 1800,
                        "concurrency_key": "sync-cycle",
                    }
                ],
            }
        )
    elif kind == "endpoint":
        files["components/endpoint.wasm"] = load_fixture_wasm("endpoint")
        components.append(
            {
                "id": "endpoint",
                "entry": "components/endpoint.wasm",
                "world": "crawler:plugin/endpoint-plugin@1.0.0",
                "permissions": ["kv"],
                "endpoints": [
                    {
                        "id": "ping",
                        "method": "POST",
                        "path": "/ping",
                        "auth": "admin",
                        "input_schema": "schemas/config.schema.json",
                        "capabilities": [],
                    }
                ],
            }
        )
    elif kind == "indexer":
        files["components/indexer.wasm"] = load_fixture_wasm("indexer")
        files["schemas/site.schema.json"] = (
            json.dumps(_DEFAULT_CONFIG_SCHEMA, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
        if eff_cred_binding is None:
            eff_cred_binding = "invocation-site"
        components.append(
            {
                "id": "indexer",
                "entry": "components/indexer.wasm",
                "world": "crawler:plugin/indexer-plugin@1.0.0",
                "permissions": ["http", "http:site-auth", "read:sites", "indexer:provide", "kv"],
                "indexers": [
                    {
                        "id": "fixture",
                        "name": "Fixture Indexer",
                        "kind": "pt",
                        "capabilities": [
                            "search",
                            "rss",
                            "detail",
                            "resolve-download",
                            "user-stats",
                            "categories",
                        ],
                        "site_schema": "schemas/site.schema.json",
                    }
                ],
            }
        )
    elif kind == "maintenance":
        files["components/maintenance.wasm"] = load_fixture_wasm("maintenance")
        if eff_cred_binding is None:
            eff_cred_binding = "invocation-site"
        components.append(
            {
                "id": "maintenance",
                "entry": "components/maintenance.wasm",
                "world": "crawler:plugin/maintenance-plugin@1.0.0",
                "permissions": ["maintenance:provide", "http:site-auth", "kv"],
                "maintenance": [
                    {
                        "id": "e2e-maint",
                        "actions": ["verify-session", "check-in"],
                    }
                ],
            }
        )
    elif kind == "yaml-only":
        include_site_yaml = True
    elif kind == "ui-only":
        include_ui = True
    else:
        raise ValueError(f"Unsupported plugin fixture kind: {kind!r}")

    if extra_components:
        components.extend(extra_components)

    if include_site_yaml:
        if not site_id or "/" in site_id or "\\" in site_id or ".." in site_id:
            raise ValueError(f"Invalid site_id: {site_id!r}")
        site_rel = f"sites/{site_id}.yaml"
        sites = [site_rel]
        files[site_rel] = (
            f"id: {site_id}\n"
            "name: E2E Demo Plugin Site\n"
            f"framework: {site_framework}\n"
            "render: false\n"
            "search:\n"
            "  path: /torrents.php\n"
            "  query_param: search\n"
            "list:\n"
            "  item: \"tr.torrent\"\n"
            "fields:\n"
            "  title:\n"
            "    selector: \"a.name\"\n"
            "    attr: title\n"
            "  enclosure:\n"
            "    selector: \"a.download\"\n"
            "    attr: href\n"
            "  detail:\n"
            "    selector: \"a.name\"\n"
            "    attr: href\n"
            "  size:\n"
            "    selector: \"td.size\"\n"
            "  seeders:\n"
            "    selector: \"td.seeders\"\n"
            "  leechers:\n"
            "    selector: \"td.leechers\"\n"
            "  snatched:\n"
            "    selector: \"td.snatched\"\n"
            "  upload_time:\n"
            "    selector: \"td.upload_time\"\n"
            "  category:\n"
            "    selector: \"td.category\"\n"
            "  free:\n"
            "    selector: \"img.pro_free\"\n"
            "  hr:\n"
            "    selector: \"img.hitandrun\"\n"
            "rss:\n"
            "  item: item\n"
            "  title:\n"
            "    selector: title\n"
            "  enclosure:\n"
            "    selector: enclosure\n"
            "    attr: url\n"
            "  id:\n"
            "    selector: guid\n"
            "  size:\n"
            "    selector: enclosure\n"
            "    attr: length\n"
            "  seeders:\n"
            "    selector: seeders\n"
            "  leechers:\n"
            "    selector: leechers\n"
            "  snatched:\n"
            "    selector: snatched\n"
            "  upload_time:\n"
            "    selector: pubDate\n"
            "  category:\n"
            "    selector: category\n"
            "  free:\n"
            "    selector: free\n"
            "  hr:\n"
            "    selector: hr\n"
        ).encode("utf-8")

    if include_ui or ui_overrides is not None or extra_ui_assets is not None:
        default_remote_name = plugin_id.replace(".", "_").replace("-", "_")
        eff_remote_name = (
            ui_overrides.get("remote_name", default_remote_name)
            if ui_overrides
            else default_remote_name
        )
        files.update(
            build_ui_asset_bundle(
                remote_name=eff_remote_name,
                extra_assets=extra_ui_assets,
            )
        )
        ui_cfg = {
            "kind": "federation-react",
            "contract": "1.0.0",
            "remote_name": eff_remote_name,
            "entry": "ui/assets/remoteEntry.js",
            "asset_manifest": "ui/asset-manifest.json",
            "react": "19.3.0",
            "react_dom": "19.3.0",
            "capabilities": ["read:sites"],
            "surfaces": [
                {
                    "id": "main",
                    "kind": "config",
                    "expose": "./Config",
                    "title": "Settings",
                }
            ],
        }
        if ui_overrides:
            ui_cfg.update(ui_overrides)

    if extra_files:
        files.update(extra_files)

    if manifest_override is not None:
        manifest_toml = manifest_override
    else:
        toml_kwargs: Dict[str, Any] = {
            "plugin_id": plugin_id,
            "version": version,
            "state_schema": state_schema,
            "components": components,
            "sites": sites,
            "ui": ui_cfg,
            "credential_binding": eff_cred_binding,
            "http_rules": http_rules,
            "limits": limits,
        }
        if authors is not None:
            toml_kwargs["authors"] = authors
        if manifest_overrides:
            toml_kwargs.update(manifest_overrides)
        manifest_toml = build_plugin_toml(**toml_kwargs)
    files["plugin.toml"] = manifest_toml.encode("utf-8")

    ext_map = external_attr_overrides or {}
    comp_map = compress_overrides or {}
    sorted_entries = [
        (
            arcname,
            files[arcname],
            ext_map.get(arcname),
            comp_map.get(arcname),
        )
        for arcname in sorted(files.keys())
    ]
    raw_bytes = _write_deterministic_zip(sorted_entries)
    sha_bytes = hashlib.sha256(raw_bytes).digest()
    return CmpluginPackage(
        bytes=raw_bytes,
        sha256_hex=sha_bytes.hex(),
        sha256_bytes=sha_bytes,
        plugin_id=plugin_id,
        version=version,
        manifest_toml=manifest_toml,
        entries=tuple(sorted(files.keys())),
    )


def _set_zip_encrypted_flag(zip_bytes: bytes) -> bytes:
    """Flip the general-purpose encryption bit (0x0001) on the first ZIP entry."""
    data = bytearray(zip_bytes)
    lfh = data.find(b"PK\x03\x04")
    cdh = data.find(b"PK\x01\x02")
    if lfh == -1 or cdh == -1:
        raise ValueError("Not a valid ZIP archive")
    lfh_flags = struct.unpack_from("<H", data, lfh + 6)[0] | 0x0001
    struct.pack_into("<H", data, lfh + 6, lfh_flags)
    cdh_flags = struct.unpack_from("<H", data, cdh + 8)[0] | 0x0001
    struct.pack_into("<H", data, cdh + 8, cdh_flags)
    return bytes(data)


def _set_zip_multi_disk_or_zip64(zip_bytes: bytes) -> bytes:
    """Patch EOCD disk number and ZIP64 entry sentinel in a ZIP archive."""
    data = bytearray(zip_bytes)
    eocd = data.rfind(b"PK\x05\x06")
    if eocd == -1 or eocd + 22 > len(data):
        raise ValueError("Missing ZIP EOCD")
    struct.pack_into("<HH", data, eocd + 4, 1, 1)
    struct.pack_into("<H", data, eocd + 10, 0xFFFF)
    return bytes(data)


def _corrupt_zip_crc32(zip_bytes: bytes) -> bytes:
    """Corrupt the first entry CRC-32 consistently in both LFH and CDH."""
    data = bytearray(zip_bytes)
    lfh = data.find(b"PK\x03\x04")
    cdh = data.find(b"PK\x01\x02")
    if lfh == -1 or cdh == -1:
        raise ValueError("Not a valid ZIP archive")
    lfh_crc = struct.unpack_from("<I", data, lfh + 14)[0] ^ 0xDEADBEEF
    struct.pack_into("<I", data, lfh + 14, lfh_crc)
    struct.pack_into("<I", data, cdh + 16, lfh_crc)
    return bytes(data)


NEGATIVE_CMPLUGIN_KINDS = (
    "path_traversal",
    "absolute_path",
    "symlink",
    "duplicate_paths",
    "case_collision",
    "encrypted_entry",
    "winzip_aes_encrypted",
    "multi_disk_or_zip64",
    "crc_mismatch",
    "zip_bomb",
    "undeclared_executable",
    "undeclared_wasm",
    "empty_package",
    "invalid_toml_unknown_field",
    "invalid_toml_bad_semver",
    "invalid_toml_bad_api",
    "invalid_toml_bad_world",
    "invalid_toml_phase2_world",
    "invalid_toml_reserved_id",
    "invalid_toml_html_metadata",
    "invalid_toml_cross_world_table",
    "invalid_toml_unsupported_hook",
    "invalid_hook_priority_oob",
    "invalid_http_wildcard_origin",
    "invalid_yaml_builtin_collision",
    "invalid_schema_remote_ref",
    "invalid_schema_recursive_ref",
    "invalid_schema_bad_widget",
    "invalid_schema_cron_format",
    "invalid_schema_secret_in_array",
    "invalid_schema_additional_props_true",
    "invalid_schema_depth_exceeded",
    "invalid_ui_html",
    "invalid_ui_map",
    "invalid_ui_wrong_sha256",
    "invalid_ui_uppercase_sha256",
    "invalid_ui_mime_mismatch",
    "invalid_ui_self_listed_manifest",
    "invalid_ui_manifest_version",
    "invalid_ui_missing_entry_in_manifest",
    "invalid_ui_undeclared_asset",
    "invalid_ui_too_many_files",
    "invalid_ui_oversized_file",
    "invalid_ui_contract_mismatch",
    "invalid_ui_react_mismatch",
    "invalid_ui_reserved_remote_name",
    "invalid_ui_too_many_surfaces",
    "invalid_ui_duplicate_config_surface",
    "invalid_ui_invalid_icon",
    "bundled_pubkey_zip",
)


def build_negative_cmplugin(kind: str) -> bytes:
    """Generate a deterministic malformed/malicious .cmplugin archive for negative tests."""
    base_wasm = load_fixture_wasm("policy")
    base_schema = (json.dumps(_DEFAULT_CONFIG_SCHEMA) + "\n").encode("utf-8")
    base_toml = build_plugin_toml(
        components=[
            {
                "id": "policy",
                "entry": "components/policy.wasm",
                "world": "crawler:plugin/hook-plugin@1.0.0",
                "permissions": ["hooks:search.after"],
                "hooks": [{"id": "search.after", "priority": 400}],
            }
        ]
    ).encode("utf-8")

    if kind == "path_traversal":
        return _write_deterministic_zip(
            [
                ("plugin.toml", base_toml, None, None),
                ("schemas/config.schema.json", base_schema, None, None),
                ("components/policy.wasm", base_wasm, None, None),
                ("../evil.wasm", b"evil", None, None),
            ]
        )
    if kind == "absolute_path":
        return _write_deterministic_zip(
            [
                ("plugin.toml", base_toml, None, None),
                ("schemas/config.schema.json", base_schema, None, None),
                ("components/policy.wasm", base_wasm, None, None),
                ("/abs.wasm", b"evil", None, None),
            ]
        )
    if kind == "symlink":
        return _write_deterministic_zip(
            [
                ("plugin.toml", base_toml, None, None),
                ("schemas/config.schema.json", base_schema, None, None),
                ("components/policy.wasm", base_wasm, 0o120777 << 16, None),
            ]
        )
    if kind == "duplicate_paths":
        return _write_deterministic_zip(
            [
                ("plugin.toml", base_toml, None, None),
                ("plugin.toml", base_toml, None, None),
                ("schemas/config.schema.json", base_schema, None, None),
                ("components/policy.wasm", base_wasm, None, None),
            ],
            allow_duplicates=True,
        )
    if kind == "case_collision":
        return _write_deterministic_zip(
            [
                ("plugin.toml", base_toml, None, None),
                ("Plugin.toml", base_toml, None, None),
                ("schemas/config.schema.json", base_schema, None, None),
                ("components/policy.wasm", base_wasm, None, None),
            ]
        )
    if kind == "encrypted_entry":
        raw = _write_deterministic_zip(
            [
                ("plugin.toml", base_toml, None, None),
                ("schemas/config.schema.json", base_schema, None, None),
                ("components/policy.wasm", base_wasm, None, None),
            ]
        )
        return _set_zip_encrypted_flag(raw)
    if kind == "winzip_aes_encrypted":
        aes_extra = struct.pack("<HH", 0x9901, 7) + b"\x02\x00AE\x03\x08\x00"
        return _write_deterministic_zip(
            [
                ("plugin.toml", base_toml, None, None),
                ("schemas/config.schema.json", base_schema, None, None),
                ("components/policy.wasm", base_wasm, None, None),
            ],
            extra_field_overrides={"components/policy.wasm": aes_extra},
        )
    if kind == "multi_disk_or_zip64":
        return _set_zip_multi_disk_or_zip64(build_cmplugin(kind="hook").bytes)
    if kind == "crc_mismatch":
        return _corrupt_zip_crc32(build_cmplugin(kind="hook").bytes)
    if kind == "zip_bomb":
        bomb = b"\x00" * (12 * 1024 * 1024)
        return _write_deterministic_zip(
            [
                ("plugin.toml", base_toml, None, None),
                ("schemas/config.schema.json", base_schema, None, None),
                ("components/policy.wasm", base_wasm, None, None),
                ("README.md", bomb, None, zipfile.ZIP_DEFLATED),
            ]
        )
    if kind == "undeclared_executable":
        return build_cmplugin(
            extra_files={"bin/run.sh": b"#!/bin/sh\necho pwn\n"},
            external_attr_overrides={"bin/run.sh": 0o100755 << 16},
        ).bytes
    if kind == "undeclared_wasm":
        return build_cmplugin(extra_files={"components/extra.wasm": base_wasm}).bytes
    if kind == "empty_package":
        empty_toml = build_plugin_toml(components=[], sites=None, ui=None)
        return _write_deterministic_zip(
            [
                ("plugin.toml", empty_toml.encode("utf-8"), None, None),
                ("schemas/config.schema.json", base_schema, None, None),
            ]
        )
    if kind == "invalid_toml_unknown_field":
        bad = build_plugin_toml(
            extra_top_level={"unexpected_secret_field": "boom"},
            components=[
                {
                    "id": "policy",
                    "entry": "components/policy.wasm",
                    "world": "crawler:plugin/hook-plugin@1.0.0",
                    "permissions": ["hooks:search.after"],
                    "hooks": [{"id": "search.after", "priority": 400}],
                }
            ],
        )
        return build_cmplugin(manifest_override=bad).bytes
    if kind == "invalid_toml_bad_semver":
        bad = build_plugin_toml(
            version=">=1.0",
            components=[
                {
                    "id": "policy",
                    "entry": "components/policy.wasm",
                    "world": "crawler:plugin/hook-plugin@1.0.0",
                    "permissions": ["hooks:search.after"],
                    "hooks": [{"id": "search.after", "priority": 400}],
                }
            ],
        )
        return build_cmplugin(manifest_override=bad).bytes
    if kind == "invalid_toml_bad_api":
        bad = build_plugin_toml(
            api="^1.0",
            components=[
                {
                    "id": "policy",
                    "entry": "components/policy.wasm",
                    "world": "crawler:plugin/hook-plugin@1.0.0",
                    "permissions": ["hooks:search.after"],
                    "hooks": [{"id": "search.after", "priority": 400}],
                }
            ],
        )
        return build_cmplugin(manifest_override=bad).bytes
    if kind == "invalid_toml_bad_world":
        bad = build_plugin_toml(
            components=[
                {
                    "id": "policy",
                    "entry": "components/policy.wasm",
                    "world": "crawler:plugin/unknown-world@1.0.0",
                    "permissions": ["hooks:search.after"],
                }
            ],
        )
        return build_cmplugin(manifest_override=bad).bytes
    if kind == "invalid_toml_phase2_world":
        bad = build_plugin_toml(
            components=[
                {
                    "id": "lists",
                    "entry": "components/policy.wasm",
                    "world": "crawler:plugin/list-plugin@1.0.0",
                    "permissions": ["kv"],
                }
            ],
        )
        return build_cmplugin(manifest_override=bad).bytes
    if kind == "invalid_toml_reserved_id":
        return build_cmplugin(plugin_id="installed").bytes
    if kind == "invalid_toml_html_metadata":
        return build_cmplugin(
            manifest_overrides={"name": "Bad <script>alert(1)</script> Plugin"}
        ).bytes
    if kind == "invalid_toml_cross_world_table":
        bad = build_plugin_toml(
            components=[
                {
                    "id": "policy",
                    "entry": "components/policy.wasm",
                    "world": "crawler:plugin/hook-plugin@1.0.0",
                    "permissions": ["hooks:search.after"],
                    "hooks": [{"id": "search.after", "priority": 400}],
                    "tasks": [
                        {
                            "id": "rogue-task",
                            "input_schema": "schemas/config.schema.json",
                            "seconds": 300,
                        }
                    ],
                }
            ],
        )
        return build_cmplugin(manifest_override=bad).bytes
    if kind == "invalid_toml_unsupported_hook":
        return build_cmplugin(
            kind="hook",
            hooks=[{"id": "playback.progress", "priority": 400}],
            hook_permissions=["hooks:search.after"],
        ).bytes
    if kind == "invalid_hook_priority_oob":
        return build_cmplugin(
            kind="hook",
            hooks=[{"id": "search.after", "priority": 1001}],
        ).bytes
    if kind == "invalid_http_wildcard_origin":
        return build_cmplugin(
            kind="hook",
            http_rules=[
                {
                    "origin": "https://*.example.com",
                    "path_prefix": "/",
                    "methods": ["GET"],
                }
            ],
        ).bytes
    if kind == "invalid_yaml_builtin_collision":
        return build_cmplugin(kind="yaml-only", site_id="mteam").bytes
    if kind == "invalid_schema_remote_ref":
        bad_schema = {
            "type": "object",
            "additionalProperties": False,
            "properties": {"bad": {"$ref": "https://evil.invalid/schema.json"}},
        }
        return build_cmplugin(config_schema_obj=bad_schema).bytes
    if kind == "invalid_schema_recursive_ref":
        bad_schema = {
            "type": "object",
            "additionalProperties": False,
            "$defs": {
                "node": {
                    "type": "object",
                    "properties": {"next": {"$ref": "#/$defs/node"}},
                }
            },
            "properties": {"root": {"$ref": "#/$defs/node"}},
        }
        return build_cmplugin(config_schema_obj=bad_schema).bytes
    if kind == "invalid_schema_bad_widget":
        bad_schema = {
            "type": "object",
            "additionalProperties": False,
            "properties": {
                "x": {"type": "string", "x-ui": {"widget": "raw-html-eval"}}
            },
        }
        return build_cmplugin(config_schema_obj=bad_schema).bytes
    if kind == "invalid_schema_cron_format":
        bad_schema = {
            "type": "object",
            "additionalProperties": False,
            "properties": {"sched": {"type": "string", "format": "cron"}},
        }
        return build_cmplugin(config_schema_obj=bad_schema).bytes
    if kind == "invalid_schema_secret_in_array":
        bad_schema = {
            "type": "object",
            "additionalProperties": False,
            "properties": {
                "tokens": {
                    "type": "array",
                    "items": {"type": "string", "writeOnly": True},
                }
            },
        }
        return build_cmplugin(config_schema_obj=bad_schema).bytes
    if kind == "invalid_schema_additional_props_true":
        bad_schema = {
            "type": "object",
            "additionalProperties": True,
            "properties": {"x": {"type": "string"}},
        }
        return build_cmplugin(config_schema_obj=bad_schema).bytes
    if kind == "invalid_schema_depth_exceeded":
        cur: Dict[str, Any] = {"type": "string"}
        for idx in range(18):
            cur = {
                "type": "object",
                "additionalProperties": False,
                "properties": {f"k{idx}": cur},
            }
        return build_cmplugin(config_schema_obj=cur).bytes
    if kind == "invalid_ui_html":
        pkg = build_cmplugin(
            kind="ui-only",
            extra_files={"ui/assets/index.html": b"<html><body>xss</body></html>"},
        )
        return pkg.bytes
    if kind == "invalid_ui_map":
        pkg = build_cmplugin(
            kind="ui-only",
            extra_files={"ui/assets/remoteEntry.js.map": b"{}"},
        )
        return pkg.bytes
    if kind == "invalid_ui_wrong_sha256":
        bundle = build_ui_asset_bundle(remote_name="org_example_e2e_policy")
        manifest_obj = json.loads(bundle["ui/asset-manifest.json"].decode("utf-8"))
        manifest_obj["files"][0]["sha256"] = "0" * 64
        bad_manifest = (
            json.dumps(manifest_obj, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
        return build_cmplugin(
            kind="ui-only",
            extra_files={"ui/asset-manifest.json": bad_manifest},
        ).bytes
    if kind == "invalid_ui_uppercase_sha256":
        bundle = build_ui_asset_bundle(remote_name="org_example_e2e_policy")
        manifest_obj = json.loads(bundle["ui/asset-manifest.json"].decode("utf-8"))
        manifest_obj["files"][0]["sha256"] = manifest_obj["files"][0]["sha256"].upper()
        bad_manifest = (
            json.dumps(manifest_obj, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
        return build_cmplugin(
            kind="ui-only",
            extra_files={"ui/asset-manifest.json": bad_manifest},
        ).bytes
    if kind == "invalid_ui_mime_mismatch":
        bundle = build_ui_asset_bundle(remote_name="org_example_e2e_policy")
        manifest_obj = json.loads(bundle["ui/asset-manifest.json"].decode("utf-8"))
        for entry in manifest_obj["files"]:
            if entry["path"].endswith(".js"):
                entry["content_type"] = "text/plain"
        bad_manifest = (
            json.dumps(manifest_obj, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
        return build_cmplugin(
            kind="ui-only",
            extra_files={"ui/asset-manifest.json": bad_manifest},
        ).bytes
    if kind == "invalid_ui_self_listed_manifest":
        self_entry = {
            "path": "ui/asset-manifest.json",
            "sha256": "a" * 64,
            "size": 10,
            "content_type": "application/json",
        }
        bundle = build_ui_asset_bundle(
            remote_name="org_example_e2e_policy",
            extra_manifest_entries=[self_entry],
        )
        return build_cmplugin(kind="ui-only", extra_files=bundle).bytes
    if kind == "invalid_ui_manifest_version":
        bundle = build_ui_asset_bundle(
            remote_name="org_example_e2e_policy",
            manifest_version=2,
        )
        return build_cmplugin(kind="ui-only", extra_files=bundle).bytes
    if kind == "invalid_ui_missing_entry_in_manifest":
        bundle = build_ui_asset_bundle(
            remote_name="org_example_e2e_policy",
            omit_from_manifest=["ui/assets/remoteEntry.js"],
        )
        return build_cmplugin(kind="ui-only", extra_files=bundle).bytes
    if kind == "invalid_ui_undeclared_asset":
        return build_cmplugin(
            kind="ui-only",
            extra_files={"ui/assets/extra.js": b"export const undeclared = true;\n"},
        ).bytes
    if kind == "invalid_ui_too_many_files":
        many: Dict[str, Tuple[bytes, str]] = {}
        for idx in range(1025):
            many[f"ui/assets/c{idx:04d}.js"] = (b"export {};\n", "application/javascript")
        bundle = build_ui_asset_bundle(extra_assets=many)
        return build_cmplugin(kind="ui-only", extra_files=bundle).bytes
    if kind == "invalid_ui_oversized_file":
        big_bundle = build_ui_asset_bundle(
            extra_assets={
                "ui/assets/huge.js": (b"a" * (8 * 1024 * 1024 + 1), "application/javascript")
            }
        )
        return build_cmplugin(
            kind="ui-only",
            extra_files=big_bundle,
            compress_overrides={"ui/assets/huge.js": zipfile.ZIP_STORED},
        ).bytes
    if kind == "invalid_ui_contract_mismatch":
        return build_cmplugin(
            kind="ui-only",
            ui_overrides={"contract": "2.0.0"},
        ).bytes
    if kind == "invalid_ui_react_mismatch":
        return build_cmplugin(
            kind="ui-only",
            ui_overrides={"react": "18.3.0"},
        ).bytes
    if kind == "invalid_ui_reserved_remote_name":
        return build_cmplugin(
            kind="ui-only",
            ui_overrides={"remote_name": "window"},
        ).bytes
    if kind == "invalid_ui_too_many_surfaces":
        surfs = [
            {
                "id": f"p-{i}",
                "kind": "page",
                "expose": f"./Page{i}",
                "title": f"Page {i}",
            }
            for i in range(17)
        ]
        return build_cmplugin(kind="ui-only", ui_overrides={"surfaces": surfs}).bytes
    if kind == "invalid_ui_duplicate_config_surface":
        return build_cmplugin(
            kind="ui-only",
            ui_overrides={
                "surfaces": [
                    {"id": "cfg-1", "kind": "config", "expose": "./Config1", "title": "Cfg 1"},
                    {"id": "cfg-2", "kind": "config", "expose": "./Config2", "title": "Cfg 2"},
                ]
            },
        ).bytes
    if kind == "invalid_ui_invalid_icon":
        return build_cmplugin(
            kind="ui-only",
            ui_overrides={
                "surfaces": [
                    {
                        "id": "main",
                        "kind": "app",
                        "expose": "./App",
                        "title": "Bad Icon",
                        "icon": "not-an-icon",
                    }
                ]
            },
        ).bytes
    if kind == "bundled_pubkey_zip":
        kp = generate_minisign_keypair(seed=b"\xaa" * 32)
        return build_cmplugin(
            kind="hook",
            extra_files={"publisher.pub": kp.public_key_text.encode("utf-8")},
        ).bytes

    raise ValueError(f"Unknown negative cmplugin kind: {kind!r}")
