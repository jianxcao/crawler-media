#!/usr/bin/env python3
"""Build bounded independent business expectations from a stopped fixture."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import subprocess
import sys
import time
import uuid
from contextlib import closing

ROOT = Path(__file__).resolve().parents[2]
TOOL_FILES = ('perfgate.py', 'perf_business.py', 'perf_business_oracle.py', 'perf_support.py',
              'perf_similar.py', 'list-routes.py')
DDL_HASHES = {
    ('old', 'app'): '2c2f1b12c06e804da679d188e9be6db8f9b66a2ed86f68c4b8f5bce15e268615',
    ('old', 'library'): '94df4d238f3f40134c08cdd5ec03cf002d7a12863c54f0c8f9ff7df6c8efb5df',
    ('new', 'app'): 'ca3ec70c1ff2e83ae0e5c8c4b81edbd7c7a40ce8ab916e3be5af2f5b433008b8',
    ('new', 'library'): '71dcdad8c0502398fb1868f239abadfa07bfc4b638bdc8f6f5c65a683a065e13',
}


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def tree(root):
    result = {}
    for path in [root, *root.rglob('*')]:
        mode = path.lstat().st_mode
        if stat.S_ISDIR(mode):
            result[str(path.relative_to(root))] = 'directory'
        elif stat.S_ISREG(mode):
            result[str(path.relative_to(root))] = digest(path.read_bytes())
        else:
            raise ValueError('baseline contains a symlink or special file')
    return result


def verified(value):
    return {'status': 'verified', 'value': value}


def unknown(reason):
    return {'status': 'unverified', 'reason': reason}


def connect(path, app_id, version):
    db = sqlite3.connect(path.as_uri() + '?mode=ro&immutable=1', uri=True)
    db.row_factory = sqlite3.Row
    if db.execute('PRAGMA application_id').fetchone()[0] != app_id or db.execute('PRAGMA user_version').fetchone()[0] not in version:
        db.close()
        raise ValueError('unsupported database layout')
    return db


def schema_rows(db):
    return [tuple(row) for row in db.execute("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")]


def verify_schema(db, layout, name):
    path = f'crates/store/src/schema/v1/{name}.sql'
    if layout == 'old':
        raw = subprocess.run(['git', 'show', f'ce3f189e:{path}'], cwd=ROOT,
                             check=True, capture_output=True).stdout
    else:
        raw = (ROOT / path).read_bytes()
    if digest(raw) != DDL_HASHES[layout, name]:
        raise ValueError('official DDL digest mismatch')
    with closing(sqlite3.connect(':memory:')) as expected:
        expected.executescript(raw.decode('utf-8'))
        if schema_rows(db) != schema_rows(expected):
            raise ValueError('database schema differs from official DDL')
    if db.execute('PRAGMA foreign_key_check').fetchone() is not None:
        raise ValueError('database foreign key violation')


def work_id(value):
    parsed = uuid.UUID(value)
    if str(parsed) != value:
        raise ValueError('noncanonical media identifier')
    return 'w1_' + parsed.hex


def field_names(fields, kind):
    field = fields.get(kind, {})
    if not isinstance(field, dict):
        raise ValueError('invalid metadata field')
    if field.get('state') == 'empty':
        if 'value' in field:
            raise ValueError('explicit empty category contains a value')
        return set()
    if field.get('state') != 'value' or not isinstance(field.get('value'), list):
        raise ValueError('category field is missing or unsupported')
    values = field['value']
    names = [entry['name'] for entry in values] if kind == 'genres' else values
    if any(not isinstance(name, str) or not name or name != name.strip() for name in names):
        raise ValueError('invalid category name')
    return set(names)


def display_names(rows, locale, kind, composed_names, composed_locale):
    if not rows:
        if composed_locale:
            raise ValueError('provider facts have no settled display row')
        return set()
    chosen = next((row for row in rows if row['locale'] == locale), rows[0])
    details = json.loads(chosen['details_json'])
    if details.get('version') != 1 or not isinstance(details.get('fields'), dict):
        raise ValueError('unsupported persisted display fields')
    names = field_names(details['fields'], kind)
    if kind == 'genres':
        stored = json.loads(chosen['genres_json'])
        stored_names = field_names({'genres': {'state': 'value', 'value': stored}}, kind)
        if (chosen['locale'] != composed_locale or composed_names != names
                or stored_names != names):
            raise ValueError('genre source and persisted display disagree')
    return names


def categories(db, owners, kind):
    """Derive names only when each source and its locale are unambiguous."""
    result = {}
    for media, libraries in owners.items():
        try:
            sources = db.execute("SELECT locale,normalized_json,selection_origins_json FROM media_metadata_sources WHERE media_id=? AND entity_scope='work' AND season=-1 AND episode=-1", (media,)).fetchall()
            overrides = db.execute("SELECT locale,fields_json FROM media_metadata_overrides WHERE media_id=? AND entity_scope='work' AND season=-1 AND episode=-1", (media,)).fetchall()
            display = db.execute('SELECT locale,details_json,genres_json FROM media_metadata WHERE media_id=? ORDER BY locale', (media,)).fetchall()
            if len(sources) > 1 or len(overrides) > 1:
                raise ValueError('multiple providers or overrides')
            adopted = {}
            if sources:
                if '$pending_target' in json.loads(sources[0]['selection_origins_json']):
                    raise ValueError('pending provider selection')
                adopted = json.loads(sources[0]['normalized_json'])
            if overrides:
                if sources and sources[0]['locale'] != overrides[0]['locale']:
                    raise ValueError('multiple adopted locales')
                override = json.loads(overrides[0]['fields_json']).get(kind, {})
                if override and override.get('state') != 'missing':
                    adopted[kind] = override
            adopted_locale = sources[0]['locale'] if sources else overrides[0]['locale'] if overrides else None
            adopted_names = field_names(adopted, kind) if adopted_locale and kind == 'genres' else None
            names = set()
            for lib in libraries:
                if adopted_locale and lib['display_locale'] != adopted_locale:
                    raise ValueError('adopted locale selection is unsupported')
                inputs = db.execute("SELECT locale,input_key,parse_state,fields_json FROM library_metadata_inputs WHERE library_id=? AND media_id=? AND entity_scope='work' AND season=-1 AND episode=-1", (lib['library_id'], media)).fetchall()
                if len(inputs) > 1 or any(row['parse_state'] != 'valid' or row['locale'] != lib['display_locale'] for row in inputs):
                    raise ValueError('NFO input selection is unsupported')
                if inputs:
                    names.update(field_names(json.loads(inputs[0]['fields_json']), kind))
                # Studios use persisted display facts: record_scraped_display
                # can write them independently of provider/override composition.
                names.update(display_names(display, lib['display_locale'], kind,
                                           adopted_names, adopted_locale))
                if not inputs and not display:
                    raise ValueError('category field is missing')
        except (ValueError, TypeError, KeyError, AttributeError):
            return unknown('category input is missing, ambiguous or unsupported')
        for name in names:
            result[name] = result.get(name, 0) + 1
    return verified(dict(sorted(result.items())))


def derive(app, db, manifest):
    users = {r['id']: dict(r) for r in app.execute('SELECT id,login,enabled,role FROM users')}
    libraries = [dict(r) for r in db.execute("SELECT library_id,kind,lifecycle_state,access_mode,admin_visible,member_ids_json,COALESCE(NULLIF(TRIM(display_locale),''),(SELECT default_locale FROM library_query_config WHERE id=1)) AS display_locale FROM libraries")]
    removals = {r['library_id'] for r in db.execute("SELECT library_id FROM library_removals WHERE state != 'succeeded'")}
    for lib in libraries:
        lib['members'] = json.loads(lib['member_ids_json'])
        if (not isinstance(lib['members'], list) or any(x not in users for x in lib['members'])
                or lib['access_mode'] not in ('everyone', 'selected') or lib['admin_visible'] not in (0, 1)):
            raise ValueError('unsupported library access facts')
    media = {r['media_id']: dict(r) for r in db.execute('SELECT media_id,kind,identity_state FROM media')}
    files = [dict(r) for r in db.execute('SELECT file_id,library_id,media_id,kind,presence_state,publication_state FROM library_files')]
    units = [dict(r) for r in db.execute('SELECT file_id,library_id,media_id,season,episode FROM library_file_units')]
    result = {}
    for principal in manifest['users']:
        user = users.get(principal['id'])
        if not user or user['login'] != principal['login'] or user['enabled'] != 1 or user['role'] not in ('member', 'admin'):
            raise ValueError('manifest user does not match an enabled principal')
        visible = [lib for lib in libraries if lib['library_id'] not in removals and
                   (lib['admin_visible'] if user['role'] == 'admin' else
                    lib['access_mode'] == 'everyone' or user['id'] in lib['members'])]
        reason = None
        if any(lib['lifecycle_state'] != 'ready' for lib in visible):
            reason = 'unsupported library lifecycle'
        allowed = {lib['library_id']: lib for lib in visible}
        selected = [f for f in files if f['library_id'] in allowed and f['publication_state'] == 'published']
        if any(f['presence_state'] != 'present' or f['media_id'] not in media or
               media[f['media_id']]['identity_state'] not in ('confirmed', 'provisional') or
               f['kind'] != media[f['media_id']]['kind'] or f['kind'] != allowed[f['library_id']]['kind'] for f in selected):
            reason = 'unsupported file or identity state'
        owners = {}
        for f in selected:
            owners.setdefault(f['media_id'], {})[f['library_id']] = allowed[f['library_id']]
        file_keys = {(f['file_id'], f['library_id'], f['media_id']) for f in selected}
        relevant_units = [u for u in units if (u['file_id'], u['library_id'], u['media_id']) in file_keys]
        covered = {(u['file_id'], u['library_id'], u['media_id']) for u in relevant_units}
        if covered != file_keys or any(
                (media[u['media_id']]['kind'] == 'tv' and (u['season'] < 0 or u['episode'] < 0)) or
                (media[u['media_id']]['kind'] != 'tv' and (u['season'], u['episode']) != (-1, -1))
                for u in relevant_units):
            reason = 'incomplete or unsupported file coverage'
        episodes = {(u['media_id'], u['season'], u['episode']) for u in units
                    if (u['file_id'], u['library_id'], u['media_id']) in file_keys
                    and media[u['media_id']]['kind'] == 'tv' and u['season'] >= 0 and u['episode'] >= 0}
        counts = {'MovieCount': sum(media[m]['kind'] == 'movie' for m in owners),
                  'SeriesCount': sum(media[m]['kind'] == 'tv' for m in owners),
                  'EpisodeCount': len(episodes), 'ItemCount': len(owners)}
        movie_libraries = {}
        for lib in visible:
            if lib['kind'] == 'movie':
                values = sorted(work_id(m) for m, owned in owners.items() if lib['library_id'] in owned)
                movie_libraries[lib['library_id']] = unknown(reason) if reason else verified(values)
        row = {'counts': unknown(reason) if reason else verified(counts), 'movie_libraries': movie_libraries}
        for category in ('genres', 'studios'):
            row[category] = unknown(reason) if reason else categories(db, {m: list(v.values()) for m, v in owners.items()}, category)
        result[user['id']] = row
    return result


def run(args):
    data = Path(args.data).resolve(strict=True)
    baseline = data.parent
    stage = baseline.parent
    requested_out = Path(args.out).absolute()
    out = requested_out.parent.resolve(strict=True) / requested_out.name
    if out.is_relative_to(baseline) or out == Path(args.manifest).resolve():
        raise ValueError('output must be outside source and manifest')
    if data.name != 'data' or baseline.name != 'baseline' or not (stage / 'seed-complete').is_file() or (stage / 'owner.json').exists():
        raise ValueError('fixture is not a stopped, seeded baseline')
    for name in ('app.db', 'library.db'):
        for suffix in ('-wal', '-shm', '-journal'):
            if (data / (name + suffix)).exists():
                raise ValueError('database has a write sidecar')
    before = tree(baseline)
    if before != json.loads((stage / 'baseline-manifest.json').read_text()):
        raise ValueError('baseline tree differs from its manifest')
    manifest = json.loads(Path(args.manifest).read_text())
    product = manifest['product']
    source = manifest['source_snapshot']
    source_hashes = {}
    for name in ('source_manifest', 'source_tree_manifest'):
        source_path = Path(source[name + '_path'])
        expected_hash = source[name + '_sha256']
        if (not source_path.is_absolute() or not isinstance(expected_hash, str)
                or not re.fullmatch(r'[0-9a-f]{64}', expected_hash)
                or digest(source_path.read_bytes()) != expected_hash):
            raise ValueError('source manifest binding mismatch')
        source_hashes[name + '_sha256'] = expected_hash
    if json.loads(Path(source['source_tree_manifest_path']).read_text()) != before:
        raise ValueError('source tree manifest differs from baseline')
    tool_hash = digest(canonical({name: digest((Path(__file__).parent / name).read_bytes()) for name in TOOL_FILES}))
    if manifest['tool_snapshot_sha256'] != tool_hash:
        raise ValueError('tool snapshot binding mismatch')
    protected = {Path(args.manifest).resolve(), Path(source['source_manifest_path']).resolve(),
                 Path(source['source_tree_manifest_path']).resolve(), (stage / 'seed-complete').resolve()}
    if out in protected:
        raise ValueError('output conflicts with source provenance')
    facts = {name: digest((data / name).read_bytes()) for name in ('app.db', 'library.db')}
    facts_hash = digest(canonical(facts))
    if (not re.fullmatch(r'[0-9a-f]{40}', args.commit) or product['commit'] != args.commit
            or not re.fullmatch(r'[0-9a-f]{64}', product['binary_sha256'])
            or manifest['source_snapshot']['baseline_facts_sha256'] != facts_hash):
        raise ValueError('product or source provenance mismatch')
    canonical_manifest = dict(manifest)
    canonical_manifest.pop('business_expectations', None)
    with closing(connect(data / 'app.db', 1129139248, (1,))) as app, closing(connect(data / 'library.db', 1129139249, (1, 2))) as db:
        version = db.execute('PRAGMA user_version').fetchone()[0]
        layout = 'old' if version == 1 else 'new'
        verify_schema(app, layout, 'app')
        verify_schema(db, layout, 'library')
        lease = app.execute('SELECT heartbeat_at FROM instance_lock WHERE id=1').fetchone()
        if lease and lease[0] >= int(time.time() * 1000) - 60000:
            raise ValueError('recent instance lease requires a stopped fixture')
        users = derive(app, db, manifest)
    if (tree(baseline) != before or (stage / 'owner.json').exists()
            or not (stage / 'seed-complete').is_file()):
        raise ValueError('baseline changed while reading')
    for name in ('source_manifest', 'source_tree_manifest'):
        if digest(Path(source[name + '_path']).read_bytes()) != source_hashes[name + '_sha256']:
            raise ValueError('source manifest changed while reading')
    if digest(canonical({name: digest((Path(__file__).parent / name).read_bytes()) for name in TOOL_FILES})) != tool_hash:
        raise ValueError('tool snapshot changed while reading')
    artifact = {'version': 1, 'fixture_commit': args.commit, 'users': users,
                'provenance': {'manifest_sha256': digest(canonical(canonical_manifest)),
                               'product_commit': args.commit, 'binary_sha256': product['binary_sha256'],
                               'baseline_facts_sha256': facts_hash, 'oracle_version': 1, 'contract_version': 1,
                               'tool_snapshot_sha256': tool_hash, **source_hashes}}
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, 'O_NOFOLLOW', 0)
    with os.fdopen(os.open(out, flags, 0o600), 'wb') as output:
        output.write(canonical(artifact) + b'\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('data', 'manifest', 'commit', 'out'):
        parser.add_argument('--' + option, required=True)
    try:
        run(parser.parse_args())
    except (OSError, ValueError, KeyError, TypeError, sqlite3.Error, subprocess.SubprocessError) as error:
        print(f'oracle: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
