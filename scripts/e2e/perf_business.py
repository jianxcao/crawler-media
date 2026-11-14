"""Offline-bound business assertions for the six Jellyfin performance cases."""
import hashlib
import json
from pathlib import Path
import re
import urllib.parse
import uuid

CASES = {
    'jellyfin-genres': 'genres',
    'jellyfin-studios': 'studios',
    'jellyfin-items-counts': 'counts',
    'jellyfin-users-user-id-items-counts': 'counts',
    'jellyfin-users-user-id-items': 'items',
    'jellyfin-users-user-id-items-latest': 'latest',
}
URLS = {
    'jellyfin-genres': '/Genres',
    'jellyfin-studios': '/Studios',
    'jellyfin-items-counts': '/Items/Counts',
    'jellyfin-users-user-id-items-counts': '/Users/{user}/Items/Counts',
    'jellyfin-users-user-id-items': '/Users/{user}/Items?ParentId={movie_library}&Limit=50',
    'jellyfin-users-user-id-items-latest': '/Users/{user}/Items/Latest?ParentId={movie_library}&Limit=50',
}
COUNT_FIELDS = ('MovieCount', 'SeriesCount', 'EpisodeCount', 'ItemCount')
HEX64 = re.compile(r'[0-9a-f]{64}\Z')
COMMIT = re.compile(r'[0-9a-f]{40}\Z')
WORK = re.compile(r'w1_[0-9a-f]{32}\Z')
TOOL_FILES = ('perfgate.py', 'perf_business.py', 'perf_business_oracle.py', 'perf_support.py',
              'perf_similar.py', 'list-routes.py')


class Unverified(ValueError):
    pass


def digest(value):
    return hashlib.sha256(value).hexdigest()


def nonnegative(value):
    return type(value) is int and value >= 0


def tool_snapshot_sha256():
    root = Path(__file__).resolve().parent
    mapping = {name: digest((root / name).read_bytes()) for name in TOOL_FILES}
    return digest(json.dumps(mapping, sort_keys=True, separators=(',', ':')).encode())


def case_contract(raw, selected_ids):
    config = json.loads(raw)
    for case in config['cases']:
        if case.get('id') != 'self-fs-browse':
            continue
        url = case.get('url')
        if not isinstance(url, str):
            continue
        prefix, separator, query_fragment = url.partition('?')
        if prefix != '/api/v1/fs/browse' or not separator:
            continue
        query, fragment_marker, fragment = query_fragment.partition('#')
        params = query.split('&')
        paths = [index for index, param in enumerate(params) if param.startswith('path=')]
        if len(paths) == 1:
            index = paths[0]
            path = urllib.parse.unquote(params[index][5:])
            if path.startswith('/'):
                params[index] = 'path=<absolute-root>'
                case['url'] = prefix + '?' + '&'.join(params) + fragment_marker + fragment
    normalized = json.dumps(config, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()
    return {'actual_sha256': digest(raw), 'normalized_sha256': digest(normalized),
            'normalization_version': 1, 'selected_case_ids': selected_ids}


def load(manifest):
    if not isinstance(manifest, dict):
        raise Unverified('manifest invalid')
    descriptor = manifest.get('business_expectations')
    if not isinstance(descriptor, dict) or type(descriptor.get('version')) is not int or descriptor['version'] != 1:
        raise Unverified('business expectation descriptor missing or unsupported')
    path, expected_hash = descriptor.get('path'), descriptor.get('sha256')
    if not isinstance(path, str) or not Path(path).is_absolute() or not isinstance(expected_hash, str) or not HEX64.fullmatch(expected_hash):
        raise Unverified('business expectation descriptor invalid')
    try:
        raw = Path(path).read_bytes()
        artifact = json.loads(raw)
    except (OSError, ValueError, UnicodeError) as error:
        raise Unverified('business expectation artifact unreadable') from error
    if (digest(raw) != expected_hash or not isinstance(artifact, dict)
            or type(artifact.get('version')) is not int or artifact['version'] != 1):
        raise Unverified('business expectation artifact digest or version invalid')
    provenance = artifact.get('provenance')
    product, source = manifest.get('product'), manifest.get('source_snapshot')
    if not isinstance(provenance, dict) or not isinstance(product, dict) or not isinstance(source, dict):
        raise Unverified('business provenance missing')
    canonical = dict(manifest)
    canonical.pop('business_expectations', None)
    canonical_hash = digest(json.dumps(canonical, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode())
    commit, binary, facts = product.get('commit'), product.get('binary_sha256'), source.get('baseline_facts_sha256')
    try:
        for name in ('source_manifest', 'source_tree_manifest'):
            path, expected = source.get(name + '_path'), source.get(name + '_sha256')
            if (not isinstance(path, str) or not Path(path).is_absolute()
                    or not isinstance(expected, str) or not HEX64.fullmatch(expected)
                    or digest(Path(path).read_bytes()) != expected
                    or provenance.get(name + '_sha256') != expected):
                raise Unverified('source snapshot binding invalid')
        tool_hash = tool_snapshot_sha256()
        if (provenance.get('tool_snapshot_sha256') != tool_hash
                or manifest.get('tool_snapshot_sha256') != tool_hash):
            raise Unverified('tool snapshot binding invalid')
    except OSError as error:
        raise Unverified('source or tool snapshot unavailable') from error
    if (not isinstance(commit, str) or not COMMIT.fullmatch(commit)
            or not isinstance(binary, str) or not HEX64.fullmatch(binary)
            or not isinstance(facts, str) or not HEX64.fullmatch(facts)
            or provenance.get('manifest_sha256') != canonical_hash
            or provenance.get('product_commit') != commit or artifact.get('fixture_commit') != commit
            or provenance.get('binary_sha256') != binary or provenance.get('baseline_facts_sha256') != facts
            or type(provenance.get('oracle_version')) is not int or provenance['oracle_version'] != 1
            or type(provenance.get('contract_version')) is not int or provenance['contract_version'] != 1):
        raise Unverified('business provenance does not match manifest')
    if not isinstance(artifact.get('users'), dict):
        raise Unverified('business users missing')
    users, libraries = manifest.get('users'), manifest.get('libraries')
    movies, series = manifest.get('movies'), manifest.get('series')
    if (not isinstance(users, list) or not users or not isinstance(libraries, dict)
            or not isinstance(libraries.get('movie'), str) or not libraries['movie']
            or not isinstance(libraries.get('tv'), str) or not libraries['tv']
            or not isinstance(movies, list) or len(movies) < 501
            or not isinstance(series, list) or not series
            or any(not isinstance(row, dict) or not isinstance(row.get('media_item_id'), str)
                   for row in movies + series)):
        raise Unverified('business manifest resources invalid')
    ids, logins = set(), set()
    for user in users:
        if (not isinstance(user, dict) or not isinstance(user.get('id'), str) or not user['id']
                or not isinstance(user.get('login'), str) or not user['login']
                or not isinstance(user.get('collections'), list) or not user['collections']
                or any(not isinstance(value, str) or not value for value in user['collections'])
                or user['id'] in ids or user['login'] in logins):
            raise Unverified('business manifest users invalid')
        ids.add(user['id'])
        logins.add(user['login'])
        for source in (manifest.get('resources', {}), user.get('resources', {}), manifest.get('samples', {})):
            if not isinstance(source, dict) or {'user', 'movie_library'} & source.keys():
                raise Unverified('business context interpolation collision')
    return artifact, dict(provenance, artifact_sha256=expected_hash)


def verify_context(manifest, user, fields, login_ids):
    if not isinstance(user.get('id'), str) or not user['id'] or any(value != user['id'] for value in login_ids):
        raise Unverified('authenticated user does not match manifest')
    for source in (manifest.get('resources', {}), user.get('resources', {}), manifest.get('samples', {})):
        if not isinstance(source, dict) or {'user', 'movie_library'} & source.keys():
            raise Unverified('business context interpolation collision')
    if fields.get('user') != user['id'] or fields.get('movie_library') != manifest['libraries']['movie']:
        raise Unverified('business context does not match manifest')


def verify_case(case):
    if (case.get('surface') != 'jellyfin' or case.get('url') != URLS[case['id']]
            or case.get('method', 'GET') != 'GET' or case.get('auth', True) is not True
            or case.get('requests') or case.get('admin') or case.get('response_contract')):
        raise Unverified('business case request does not match contract')


def expectation(artifact, case_id, user_id, library):
    kind = CASES[case_id]
    user = artifact['users'].get(user_id)
    if not isinstance(user, dict):
        raise Unverified('user expectation missing')
    if kind == 'counts':
        envelope = user.get('counts')
    elif kind in ('items', 'latest'):
        libraries = user.get('movie_libraries')
        envelope = libraries.get(library) if isinstance(libraries, dict) else None
    else:
        envelope = user.get(kind)
    if not isinstance(envelope, dict) or envelope.get('status') != 'verified' or 'value' not in envelope:
        raise Unverified('independent assertion unavailable')
    value = envelope['value']
    if kind == 'counts':
        if not isinstance(value, dict) or set(value) != set(COUNT_FIELDS) or not all(nonnegative(value[k]) for k in COUNT_FIELDS):
            raise Unverified('counts expectation invalid')
    elif kind in ('items', 'latest'):
        if (not isinstance(value, list) or any(not isinstance(v, str) or not WORK.fullmatch(v) for v in value)
                or len(value) != len(set(value))):
            raise Unverified('movie library expectation unavailable')
        value = set(value)
    else:
        if (not isinstance(value, dict) or len(value) > 100
                or any(not isinstance(k, str) or not k or not nonnegative(v) for k, v in value.items())):
            raise Unverified('category expectation unavailable or exceeds supported page')
    return value


def matches(kind, payload, expected):
    try:
        response = json.loads(payload)
        if kind == 'counts':
            return isinstance(response, dict) and all(nonnegative(response.get(k)) and response[k] == expected[k] for k in COUNT_FIELDS)
        if kind in ('items', 'latest'):
            if kind == 'items':
                if not isinstance(response, dict) or not nonnegative(response.get('TotalRecordCount')) or response['TotalRecordCount'] != len(expected):
                    return False
                response = response.get('Items')
            if not isinstance(response, list) or len(response) != min(len(expected), 50):
                return False
            ids = [item.get('Id') for item in response if isinstance(item, dict) and item.get('Type') == 'Movie']
            return len(ids) == len(response) and all(isinstance(v, str) and v in expected for v in ids) and len(set(ids)) == len(ids)
        if not isinstance(response, dict) or not nonnegative(response.get('TotalRecordCount')) or response['TotalRecordCount'] != len(expected):
            return False
        items = response.get('Items')
        if not isinstance(items, list) or len(items) != len(expected):
            return False
        actual = {}
        for item in items:
            if not isinstance(item, dict):
                return False
            name, count = item.get('Name'), item.get('ChildCount')
            if (not isinstance(name, str) or name in actual or not nonnegative(count)
                    or item.get('Type') != ('Genre' if kind == 'genres' else 'Studio')
                    or item.get('Id') != str(uuid.uuid5(uuid.NAMESPACE_DNS, name))):
                return False
            actual[name] = count
        return actual == expected
    except (ValueError, TypeError, UnicodeError):
        return False


def baseline_reason(report, current, case_contract_current):
    old = report.get('business_provenance')
    if not isinstance(old, dict) or report.get('business_status') != 'verified':
        return 'baseline business provenance missing or unverified'
    for key in ('artifact_sha256', 'manifest_sha256', 'binary_sha256', 'baseline_facts_sha256',
                'source_manifest_sha256', 'source_tree_manifest_sha256', 'tool_snapshot_sha256'):
        if not isinstance(old.get(key), str) or not HEX64.fullmatch(old[key]):
            return 'baseline business provenance invalid'
    if (not isinstance(old.get('product_commit'), str) or not COMMIT.fullmatch(old['product_commit'])
            or type(old.get('contract_version')) is not int or old['contract_version'] != current['contract_version']
            or type(old.get('oracle_version')) is not int or old['oracle_version'] != current['oracle_version']
            or old['tool_snapshot_sha256'] != current['tool_snapshot_sha256']
            or not isinstance(report.get('commit'), str) or len(report['commit']) < 7
            or not old['product_commit'].startswith(report['commit'])):
        return 'baseline business contract or oracle incompatible'
    manifest_path = report.get('business_manifest_path')
    if not isinstance(manifest_path, str) or not Path(manifest_path).is_absolute():
        return 'baseline business manifest unavailable'
    try:
        manifest = json.loads(Path(manifest_path).read_bytes())
        _, bound = load(manifest)
    except (OSError, ValueError, TypeError, AttributeError, UnicodeError):
        return 'baseline business artifact unavailable or invalid'
    if bound != old:
        return 'baseline business provenance does not match artifact'
    contract, config_path = report.get('case_contract'), report.get('case_config_path')
    if not isinstance(contract, dict) or not isinstance(config_path, str) or not Path(config_path).is_absolute():
        return 'baseline case contract missing'
    try:
        actual = case_contract(Path(config_path).read_bytes(), contract.get('selected_case_ids'))
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        return 'baseline case contract unavailable'
    if (actual != contract or type(contract.get('normalization_version')) is not int
            or contract['normalization_version'] != case_contract_current['normalization_version']
            or contract.get('selected_case_ids') != case_contract_current['selected_case_ids']
            or contract.get('normalized_sha256') != case_contract_current['normalized_sha256']):
        return 'baseline case contract differs'
    return None
