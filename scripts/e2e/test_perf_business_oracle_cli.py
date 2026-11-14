"""Offline oracle CLI contract against independently seeded raw SQLite facts.

Run only in the owner's designated test window.
"""
import hashlib
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
import uuid


HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
ORACLE = HERE / 'perf_business_oracle.py'
COMMIT = 'a' * 40
BINARY_SHA = 'b' * 64
TOOL_FILES = ('perfgate.py', 'perf_business.py', 'perf_business_oracle.py', 'perf_support.py',
              'perf_similar.py', 'list-routes.py')


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def tree(root):
    return {str(p.relative_to(root)): ('directory' if p.is_dir() else sha(p.read_bytes()))
            for p in [root, *sorted(root.rglob('*'))]}


def ddl(layout, name):
    path = f'crates/store/src/schema/v1/{name}.sql'
    if layout == 'old':
        return subprocess.run(['git', 'show', f'ce3f189e:{path}'], cwd=REPO,
                              check=True, capture_output=True, text=True).stdout
    return (REPO / path).read_text()


def uid(layout, value):
    return str(uuid.UUID(int=(0 if layout == 'old' else 256) + value))


def tool_hash():
    return sha(canonical({name: sha((HERE / name).read_bytes()) for name in TOOL_FILES}))


def old_tool_hash():
    return sha(canonical({name: sha((HERE / name).read_bytes()) for name in TOOL_FILES
                          if name != 'perf_business_oracle.py'}))


class OracleCliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='crawler-perfgate-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def fixture(self, layout, *, ambiguous=False, nonempty=False, label=None):
        stage = self.root / (label or layout)
        baseline = stage / 'baseline'
        data = baseline / 'data'
        data.mkdir(parents=True)
        ids = {name: uid(layout, n) for n, name in enumerate(
            ('alice', 'bob', 'm1', 'm2', 'tv', 'video', 'movie', 'series', 'clip',
             'pending', 'admin'), 1)}
        with sqlite3.connect(data / 'app.db') as db:
            db.executescript(ddl(layout, 'app'))
            db.execute('PRAGMA application_id=1129139248')
            db.execute('PRAGMA user_version=1')
            db.executemany('INSERT INTO users(id,login,enabled,role) VALUES(?,?,1,?)',
                           [(ids['alice'], 'alice', 'member'), (ids['bob'], 'bob', 'member'),
                            (ids['admin'], 'admin', 'admin')])
            self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])
        with sqlite3.connect(data / 'library.db') as db:
            db.executescript(ddl(layout, 'library'))
            db.execute('PRAGMA application_id=1129139249')
            db.execute('PRAGMA user_version=' + ('1' if layout == 'old' else '2'))
            for name, kind, access, members in (
                    ('m1', 'movie', 'everyone', []), ('m2', 'movie', 'selected', [ids['alice']]),
                    ('tv', 'tv', 'everyone', []), ('video', 'video', 'selected', [ids['alice']])):
                db.execute('INSERT INTO libraries(library_id,kind,name,access_mode,member_ids_json) VALUES(?,?,?,?,?)',
                           (ids[name], kind, name, access, json.dumps(members)))
                db.execute('INSERT INTO library_roots(root_id,library_id,path_key,display_path) VALUES(?,?,?,?)',
                           ('root-' + name, ids[name], '/' + name, '/' + name))
            for name, kind in (('movie', 'movie'), ('series', 'tv'), ('clip', 'video'), ('pending', 'movie')):
                db.execute('INSERT INTO media(media_id,kind,title,identity_state) VALUES(?,?,?,?)',
                           (ids[name], kind, name, 'confirmed'))
            for file_id, lib, media, kind, publication, units in (
                    ('a', 'm1', 'movie', 'movie', 'published', [(-1, -1)]),
                    ('b', 'm2', 'movie', 'movie', 'published', [(-1, -1)]),
                    ('c', 'tv', 'series', 'tv', 'published', [(1, 1), (1, 2)]),
                    ('d', 'tv', 'series', 'tv', 'published', [(1, 1)]),
                    ('e', 'video', 'clip', 'video', 'published', [(-1, -1)]),
                    ('f', 'm1', 'pending', 'movie', 'pending', [(-1, -1)])):
                path = '/' + lib + '/' + file_id
                db.execute('INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence) VALUES(?,?,?,?,?,?,?,?,?,1,?,?,?)',
                           (file_id, ids[lib], 'root-' + lib, ids[media], kind, path, path,
                            'present', publication, 'v1', 'release', 'high'))
                db.executemany('INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES(?,?,?,?,?)',
                               [(file_id, ids[lib], ids[media], season, episode) for season, episode in units])
            # Overrides outrank provider sources. Explicit Empty is distinct from Missing.
            for name in ('movie', 'series', 'clip'):
                fields = {'genres': {'state': 'empty'}, 'studios': {'state': 'empty'}}
                if nonempty and name in ('movie', 'series'):
                    fields = {'genres': {'state': 'value', 'value': [{'source': 'tmdb',
                               'source_id': '18', 'name': 'Drama', 'filter_key': 'drama'}]},
                              'studios': {'state': 'value', 'value': ['Fixture Studio']}}
                if ambiguous and name == 'series':
                    fields.pop('studios')
                db.execute('INSERT INTO media_metadata_overrides(media_id,entity_scope,season,episode,locale,fields_json,revision) VALUES(?,\'work\',-1,-1,?,?,1)',
                           (ids[name], 'zh-CN', json.dumps(fields)))
                genres = fields.get('genres', {}).get('value', [])
                db.execute('INSERT INTO media_metadata(media_id,locale,title,genres_json,details_json,revision) VALUES(?,?,?,?,?,1)',
                           (ids[name], 'zh-CN', name, json.dumps(genres),
                            json.dumps({'version': 1, 'fields': fields})))
            self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])
        (stage / 'seed-complete').write_text('complete\n')
        (stage / 'baseline-manifest.json').write_text(json.dumps(tree(baseline)))
        (stage / 'source-manifest.json').write_bytes(canonical({'fixture': layout}))
        facts = {name: sha((data / name).read_bytes()) for name in ('app.db', 'library.db')}
        manifest = {'product': {'commit': COMMIT, 'binary_sha256': BINARY_SHA},
                    'source_snapshot': {'baseline_facts_sha256': sha(canonical(facts)),
                        'source_manifest_path': str(stage / 'source-manifest.json'),
                        'source_manifest_sha256': sha((stage / 'source-manifest.json').read_bytes()),
                        'source_tree_manifest_path': str(stage / 'baseline-manifest.json'),
                        'source_tree_manifest_sha256': sha((stage / 'baseline-manifest.json').read_bytes())},
                    'tool_snapshot_sha256': tool_hash(),
                    'libraries': {'movie': ids['m1'], 'tv': ids['tv']},
                    'users': [{'id': ids[n], 'login': n} for n in ('alice', 'bob', 'admin')]}
        manifest_path = stage / 'manifest.json'
        manifest_path.write_bytes(canonical(manifest))
        return stage, baseline, manifest_path, manifest, ids

    def invoke(self, baseline, manifest, out, commit=COMMIT):
        return subprocess.run([sys.executable, str(ORACLE), '--data', str(baseline / 'data'),
                               '--manifest', str(manifest), '--commit', commit, '--out', str(out)],
                              capture_output=True, text=True, timeout=30)

    def rejected(self, result):
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("can't open file", result.stderr)
        self.assertTrue(result.stderr.strip())

    def reseal(self, stage, baseline, path, manifest):
        facts = {name: sha((baseline / 'data' / name).read_bytes())
                 for name in ('app.db', 'library.db')}
        manifest['source_snapshot']['baseline_facts_sha256'] = sha(canonical(facts))
        (stage / 'baseline-manifest.json').write_text(json.dumps(tree(baseline)))
        manifest['source_snapshot']['source_tree_manifest_sha256'] = sha(
            (stage / 'baseline-manifest.json').read_bytes())
        path.write_bytes(canonical(manifest))

    def test_both_layouts_derive_distinct_access_and_empty_categories(self):
        for layout in ('old', 'new'):
            with self.subTest(layout=layout):
                stage, baseline, path, manifest, ids = self.fixture(layout)
                before = tree(baseline)
                out = stage / 'expectations.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                artifact = json.loads(out.read_text())
                self.assertEqual(tree(baseline), before)
                self.assertEqual(artifact['version'], 1)
                self.assertEqual(artifact['fixture_commit'], COMMIT)
                provenance = artifact['provenance']
                self.assertEqual(provenance['manifest_sha256'], sha(canonical(manifest)))
                self.assertEqual(provenance['product_commit'], COMMIT)
                self.assertEqual(provenance['binary_sha256'], BINARY_SHA)
                self.assertEqual(provenance['baseline_facts_sha256'], manifest['source_snapshot']['baseline_facts_sha256'])
                for key in ('source_manifest_sha256', 'source_tree_manifest_sha256'):
                    self.assertEqual(provenance[key], manifest['source_snapshot'][key])
                self.assertEqual(provenance['tool_snapshot_sha256'], tool_hash())
                self.assertEqual((provenance['oracle_version'], provenance['contract_version']), (1, 1))
                for name, item_count in (('alice', 3), ('bob', 2), ('admin', 3)):
                    user = artifact['users'][ids[name]]
                    self.assertEqual(user['counts'], {'status': 'verified', 'value': {
                        'MovieCount': 1, 'SeriesCount': 1, 'EpisodeCount': 2, 'ItemCount': item_count}})
                    self.assertEqual(user['movie_libraries'][ids['m1']], {'status': 'verified',
                                     'value': ['w1_' + ids['movie'].replace('-', '')]})
                    self.assertEqual(user['studios'], {'status': 'verified', 'value': {}})
                    self.assertEqual(user['genres'], {'status': 'verified', 'value': {}})
                self.assertEqual(artifact['users'][ids['alice']]['movie_libraries'][ids['m2']],
                                 {'status': 'verified', 'value': ['w1_' + ids['movie'].replace('-', '')]})
                self.assertNotIn(ids['m2'], artifact['users'][ids['bob']]['movie_libraries'])
                self.assertIn(ids['m2'], artifact['users'][ids['admin']]['movie_libraries'])

    def test_published_provisional_nfo_work_is_verified(self):
        for layout in ('old', 'new'):
            with self.subTest(layout=layout):
                stage, baseline, path, manifest, ids = self.fixture(layout)
                fields = {'genres': {'state': 'value', 'value': [
                    {'source': 'nfo', 'source_id': None, 'name': 'Local Drama', 'filter_key': 'local drama'}]},
                    'studios': {'state': 'value', 'value': ['Local Studio']}}
                with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                    db.execute("UPDATE media SET identity_state='provisional' WHERE media_id=?", (ids['movie'],))
                    db.execute('DELETE FROM media_metadata_overrides WHERE media_id=?', (ids['movie'],))
                    db.execute('DELETE FROM media_metadata WHERE media_id=?', (ids['movie'],))
                    for lib in ('m1', 'm2'):
                        db.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?,?,'work',-1,-1,'zh-CN','input',?,?,'valid','nfo_v1',?,?,1)",
                                   (ids[lib], ids['movie'], 'root-' + lib, '/' + lib + '/movie.nfo',
                                    json.dumps(fields), json.dumps(['genres', 'studios'])))
                    self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])
                self.reseal(stage, baseline, path, manifest)
                out = stage / 'provisional.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                users = json.loads(out.read_text())['users']
                for name, item_count in (('alice', 3), ('bob', 2), ('admin', 3)):
                    user = users[ids[name]]
                    self.assertEqual(user['counts'], {'status': 'verified', 'value': {
                        'MovieCount': 1, 'SeriesCount': 1, 'EpisodeCount': 2, 'ItemCount': item_count}})
                    self.assertEqual(user['movie_libraries'][ids['m1']], {'status': 'verified',
                                     'value': ['w1_' + ids['movie'].replace('-', '')]})
                    self.assertEqual(user['genres'], {'status': 'verified', 'value': {'Local Drama': 1}})
                    self.assertEqual(user['studios'], {'status': 'verified', 'value': {'Local Studio': 1}})
                self.assertEqual(users[ids['alice']]['movie_libraries'][ids['m2']],
                                 {'status': 'verified', 'value': ['w1_' + ids['movie'].replace('-', '')]})
                self.assertNotIn(ids['m2'], users[ids['bob']]['movie_libraries'])

    def test_unsupported_published_identity_remains_unverified(self):
        for state in ('merging', 'retired', 'unknown'):
            with self.subTest(state=state):
                stage, baseline, path, manifest, ids = self.fixture('new', label=state)
                with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                    db.execute('PRAGMA ignore_check_constraints=ON')
                    db.execute('UPDATE media SET identity_state=?, survivor_id=? WHERE media_id=?',
                               (state, ids['pending'] if state == 'retired' else None, ids['movie']))
                self.reseal(stage, baseline, path, manifest)
                out = stage / 'unsupported.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                for user in json.loads(out.read_text())['users'].values():
                    for key in ('counts', 'genres', 'studios'):
                        self.assertEqual(user[key]['status'], 'unverified')
                    self.assertEqual(user['movie_libraries'][ids['m1']]['status'], 'unverified')

    def test_multiple_nfo_inputs_require_agreement_per_category(self):
        for layout in ('old', 'new'):
            for scenario in ('agree', 'different', 'empty', 'missing', 'invalid', 'locale'):
                with self.subTest(layout=layout, scenario=scenario):
                    stage, baseline, path, manifest, ids = self.fixture(layout, label=layout + '-' + scenario)
                    genre = {'state': 'value', 'value': [{'source': 'nfo', 'source_id': None,
                             'name': 'Local Drama', 'filter_key': 'local drama'}]}
                    studio = {'state': 'value', 'value': ['Local Studio']}
                    first = {'genres': genre, 'studios': studio}
                    second = {'genres': genre, 'studios': studio}
                    if scenario == 'different':
                        second = {'genres': {'state': 'value', 'value': [{'source': 'nfo',
                                  'source_id': None, 'name': 'Comedy', 'filter_key': 'comedy'}]},
                                  'studios': studio}
                    elif scenario == 'empty':
                        first = second = {'genres': {'state': 'empty'}, 'studios': {'state': 'empty'}}
                    elif scenario == 'missing':
                        second = {'studios': studio}
                    with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                        db.execute('DELETE FROM media_metadata_overrides WHERE media_id=?', (ids['movie'],))
                        db.execute('DELETE FROM media_metadata WHERE media_id=?', (ids['movie'],))
                        for lib in ('m1', 'm2'):
                            for key, fields in (('a', first), ('b', second)):
                                locale = 'en-US' if scenario == 'locale' and key == 'b' else 'zh-CN'
                                state = 'invalid' if scenario == 'invalid' and key == 'b' else 'valid'
                                db.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?,?,'work',-1,-1,?,?,?,?,?,'nfo_v1',?,?,1)",
                                           (ids[lib], ids['movie'], locale, key, 'root-' + lib,
                                            '/' + lib + '/' + key + '.nfo', state, json.dumps(fields), json.dumps(list(fields))))
                    self.reseal(stage, baseline, path, manifest)
                    out = stage / 'multiple-nfo.json'
                    result = self.invoke(baseline, path, out)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    for name, item_count in (('alice', 3), ('bob', 2), ('admin', 3)):
                        user = json.loads(out.read_text())['users'][ids[name]]
                        self.assertEqual(user['counts'], {'status': 'verified', 'value': {
                            'MovieCount': 1, 'SeriesCount': 1, 'EpisodeCount': 2, 'ItemCount': item_count}})
                        if scenario in ('invalid', 'locale'):
                            self.assertEqual(user['studios']['status'], 'unverified')
                        else:
                            self.assertEqual(user['studios'], {'status': 'verified', 'value':
                                             {} if scenario == 'empty' else {'Local Studio': 1}})
                        if scenario in ('different', 'missing', 'invalid', 'locale'):
                            self.assertEqual(user['genres']['status'], 'unverified')
                        else:
                            self.assertEqual(user['genres'], {'status': 'verified', 'value':
                                             {} if scenario == 'empty' else {'Local Drama': 1}})

    def test_missing_metadata_is_unverified(self):
        stage, baseline, path, _, ids = self.fixture('new', ambiguous=True)
        out = stage / 'expectations.json'
        result = self.invoke(baseline, path, out)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        artifact = json.loads(out.read_text())
        for name in ('alice', 'bob'):
            user = artifact['users'][ids[name]]
            self.assertEqual(user['studios']['status'], 'unverified')
            self.assertTrue(user['studios']['reason'])
            self.assertEqual(user['counts']['status'], 'verified')

    def test_nonempty_categories_count_distinct_visible_works(self):
        for layout in ('old', 'new'):
            with self.subTest(layout=layout):
                stage, baseline, path, _, ids = self.fixture(layout, nonempty=True)
                out = stage / 'nonempty.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                users = json.loads(out.read_text())['users']
                for name in ('alice', 'bob'):
                    self.assertEqual(users[ids[name]]['genres'],
                                     {'status': 'verified', 'value': {'Drama': 2}})
                    self.assertEqual(users[ids[name]]['studios'],
                                     {'status': 'verified', 'value': {'Fixture Studio': 2}})

    def test_nfo_and_single_provider_union_per_visible_library(self):
        for layout in ('old', 'new'):
            with self.subTest(layout=layout):
                stage, baseline, path, manifest, ids = self.fixture(layout)
                with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                    db.execute('DELETE FROM media_metadata_overrides WHERE media_id=?', (ids['movie'],))
                    for lib, name in (('m1', 'Perfgate'), ('m2', 'Other')):
                        fields = {'genres': {'state': 'value', 'value': [
                            {'source': 'nfo', 'source_id': None, 'name': name, 'filter_key': name.lower()}]},
                            'studios': {'state': 'value', 'value': ['Perfgate Studio']}}
                        db.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?,?,'work',-1,-1,'zh-CN','input',?,?, 'valid','nfo_v1',?,?,1)",
                                   (ids[lib], ids['movie'], 'root-' + lib, '/' + lib + '/movie.nfo',
                                    json.dumps(fields), json.dumps(['genres', 'studios'])))
                    fields = {'genres': {'state': 'value', 'value': [
                        {'source': 'tmdb', 'source_id': '18', 'name': 'Drama', 'filter_key': 'drama'}]},
                        'studios': {'state': 'value', 'value': ['Provider Studio']}}
                    columns = ('media_id,entity_scope,season,episode,source,locale,provider_entity_id,'
                               'provider_work_id,normalized_json,present_fields_json,selection_origins_json,fetched_at,source_revision')
                    values = (ids['movie'], 'work', -1, -1, 'tmdb', 'zh-CN', '42', '42',
                              json.dumps(fields), json.dumps(['genres', 'studios']), '{}', 1, 1)
                    if layout == 'old':
                        columns += ',preferences_json'
                        values += (json.dumps({'language_priority': [], 'cert_country_priority': []}),)
                    db.execute('INSERT INTO media_metadata_sources(' + columns + ') VALUES('
                               + ','.join('?' for _ in values) + ')', values)
                    db.execute('UPDATE media_metadata SET genres_json=?,details_json=? WHERE media_id=? AND locale=?',
                               (json.dumps(fields['genres']['value']), json.dumps({'version': 1, 'fields': fields}),
                                ids['movie'], 'zh-CN'))
                    self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])
                self.reseal(stage, baseline, path, manifest)
                out = stage / 'nfo-provider.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                users = json.loads(out.read_text())['users']
                self.assertEqual(users[ids['alice']]['genres'], {'status': 'verified', 'value':
                                 {'Drama': 1, 'Other': 1, 'Perfgate': 1}})
                self.assertEqual(users[ids['bob']]['genres'], {'status': 'verified', 'value':
                                 {'Drama': 1, 'Perfgate': 1}})
                for name in ('alice', 'bob'):
                    self.assertEqual(users[ids[name]]['studios'], {'status': 'verified', 'value':
                                     {'Perfgate Studio': 1, 'Provider Studio': 1}})

    def test_competing_provider_selection_remains_unverified(self):
        stage, baseline, path, manifest, ids = self.fixture('new')
        with sqlite3.connect(baseline / 'data' / 'library.db') as db:
            for source in ('tmdb', 'douban'):
                fields = {'genres': {'state': 'value', 'value': [{'source': source, 'source_id': '1',
                           'name': source, 'filter_key': source}]}, 'studios': {'state': 'empty'}}
                db.execute("INSERT INTO media_metadata_sources(media_id,entity_scope,season,episode,source,locale,provider_entity_id,provider_work_id,normalized_json,present_fields_json,selection_origins_json,fetched_at,source_revision) VALUES(?,'work',-1,-1,?,'zh-CN','1','1',?,'[\"genres\",\"studios\"]','{}',1,1)",
                           (ids['movie'], source, json.dumps(fields)))
        self.reseal(stage, baseline, path, manifest)
        out = stage / 'competing.json'
        result = self.invoke(baseline, path, out)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for user in json.loads(out.read_text())['users'].values():
            self.assertEqual(user['genres']['status'], 'unverified')
            self.assertTrue(user['genres']['reason'])

    def test_persisted_display_studio_and_nfo_union_distinct_owners(self):
        for layout in ('old', 'new'):
            with self.subTest(layout=layout):
                stage, baseline, path, manifest, ids = self.fixture(layout)
                with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                    fields = {'genres': {'state': 'empty'}, 'studios': {'state': 'value',
                              'value': ['NFO Studio']}}
                    for lib in ('m1', 'm2'):
                        db.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?,?,'work',-1,-1,'zh-CN','input',?,?,'valid','nfo_v1',?,?,1)",
                                   (ids[lib], ids['movie'], 'root-' + lib, '/' + lib + '/movie.nfo',
                                    json.dumps(fields), json.dumps(['genres', 'studios'])))
                    display = {'genres': {'state': 'empty'}, 'studios': {'state': 'value',
                               'value': ['Display Studio']}}
                    db.execute('UPDATE media_metadata SET details_json=? WHERE media_id=? AND locale=?',
                               (json.dumps({'version': 1, 'fields': display}), ids['movie'], 'zh-CN'))
                self.reseal(stage, baseline, path, manifest)
                out = stage / 'display.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                for user in json.loads(out.read_text())['users'].values():
                    self.assertEqual(user['studios'], {'status': 'verified', 'value':
                                     {'Display Studio': 1, 'NFO Studio': 1}})

    def test_missing_persisted_display_studio_is_unverified(self):
        for layout in ('old', 'new'):
            with self.subTest(layout=layout):
                stage, baseline, path, manifest, ids = self.fixture(layout)
                with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                    fields = {'genres': {'state': 'empty'}}
                    db.execute('UPDATE media_metadata SET details_json=? WHERE media_id=? AND locale=?',
                               (json.dumps({'version': 1, 'fields': fields}), ids['movie'], 'zh-CN'))
                self.reseal(stage, baseline, path, manifest)
                out = stage / 'missing-display.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                for user in json.loads(out.read_text())['users'].values():
                    self.assertEqual(user['studios']['status'], 'unverified')
                    self.assertTrue(user['studios']['reason'])
                    self.assertEqual(user['counts']['status'], 'verified')

    def test_nfo_only_explicit_studios_are_verified_without_any_adopted_rows(self):
        for layout in ('old', 'new'):
            with self.subTest(layout=layout):
                stage, baseline, path, manifest, ids = self.fixture(layout)
                with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                    db.execute('DELETE FROM media_metadata WHERE media_id=?', (ids['movie'],))
                    db.execute('DELETE FROM media_metadata_overrides WHERE media_id=?', (ids['movie'],))
                    fields = {'genres': {'state': 'empty'},
                              'studios': {'state': 'value', 'value': ['NFO Studio']}}
                    for lib in ('m1', 'm2'):
                        db.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?,?,'work',-1,-1,'zh-CN','input',?,?,'valid','nfo_v1',?,?,1)",
                                   (ids[lib], ids['movie'], 'root-' + lib, '/' + lib + '/movie.nfo',
                                    json.dumps(fields), json.dumps(['genres', 'studios'])))
                self.reseal(stage, baseline, path, manifest)
                out = stage / 'nfo-only.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                for user in json.loads(out.read_text())['users'].values():
                    self.assertEqual(user['studios'], {'status': 'verified',
                                                      'value': {'NFO Studio': 1}})

    def test_nfo_only_explicit_empty_studios_are_verified(self):
        for layout in ('old', 'new'):
            with self.subTest(layout=layout):
                stage, baseline, path, manifest, ids = self.fixture(layout)
                with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                    db.execute('DELETE FROM media_metadata WHERE media_id=?', (ids['movie'],))
                    db.execute('DELETE FROM media_metadata_overrides WHERE media_id=?', (ids['movie'],))
                    fields = {'genres': {'state': 'empty'}, 'studios': {'state': 'empty'}}
                    for lib in ('m1', 'm2'):
                        db.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?,?,'work',-1,-1,'zh-CN','input',?,?,'valid','nfo_v1',?,?,1)",
                                   (ids[lib], ids['movie'], 'root-' + lib, '/' + lib + '/movie.nfo',
                                    json.dumps(fields), json.dumps(['genres', 'studios'])))
                self.reseal(stage, baseline, path, manifest)
                out = stage / 'nfo-empty.json'
                result = self.invoke(baseline, path, out)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                for user in json.loads(out.read_text())['users'].values():
                    self.assertEqual(user['studios'], {'status': 'verified', 'value': {}})

    def test_genres_disagreeing_with_persisted_fields_are_unverified(self):
        for layout in ('old', 'new'):
            for source in ('provider', 'override'):
                for mismatch in ('details', 'genres_json'):
                    with self.subTest(layout=layout, source=source, mismatch=mismatch):
                        label = '-'.join((layout, source, mismatch))
                        stage, baseline, path, manifest, ids = self.fixture(layout, label=label)
                        raw_genres = [{'source': 'tmdb', 'source_id': '18',
                                       'name': 'Drama', 'filter_key': 'drama'}]
                        other_genres = [{'source': 'tmdb', 'source_id': '35',
                                         'name': 'Comedy', 'filter_key': 'comedy'}]
                        raw_fields = {'genres': {'state': 'value', 'value': raw_genres},
                                      'studios': {'state': 'empty'}}
                        with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                            if source == 'override':
                                db.execute('UPDATE media_metadata_overrides SET fields_json=? WHERE media_id=?',
                                           (json.dumps(raw_fields), ids['movie']))
                            else:
                                db.execute('DELETE FROM media_metadata_overrides WHERE media_id=?', (ids['movie'],))
                                columns = ('media_id,entity_scope,season,episode,source,locale,provider_entity_id,'
                                           'provider_work_id,normalized_json,present_fields_json,selection_origins_json,fetched_at,source_revision')
                                values = (ids['movie'], 'work', -1, -1, 'tmdb', 'zh-CN', '42', '42',
                                          json.dumps(raw_fields), json.dumps(['genres', 'studios']), '{}', 1, 1)
                                if layout == 'old':
                                    columns += ',preferences_json'
                                    values += (json.dumps({'language_priority': [], 'cert_country_priority': []}),)
                                db.execute('INSERT INTO media_metadata_sources(' + columns + ') VALUES('
                                           + ','.join('?' for _ in values) + ')', values)
                            details_genres = other_genres if mismatch == 'details' else raw_genres
                            column_genres = other_genres if mismatch == 'genres_json' else raw_genres
                            details = {'version': 1, 'fields': {'genres': {'state': 'value',
                                       'value': details_genres}, 'studios': {'state': 'empty'}}}
                            db.execute('UPDATE media_metadata SET genres_json=?,details_json=? WHERE media_id=? AND locale=?',
                                       (json.dumps(column_genres), json.dumps(details), ids['movie'], 'zh-CN'))
                        self.reseal(stage, baseline, path, manifest)
                        out = stage / 'genres-mismatch.json'
                        result = self.invoke(baseline, path, out)
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                        for user in json.loads(out.read_text())['users'].values():
                            self.assertEqual(user['genres']['status'], 'unverified')
                            self.assertTrue(user['genres']['reason'])
                            self.assertEqual(user['counts']['status'], 'verified')

    def test_empty_fields_with_nonempty_values_are_unverified(self):
        for layout in ('old', 'new'):
            for scenario in ('nfo_studios', 'display_studios', 'display_genres'):
                with self.subTest(layout=layout, scenario=scenario):
                    stage, baseline, path, manifest, ids = self.fixture(
                        layout, label=layout + '-' + scenario)
                    with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                        if scenario == 'nfo_studios':
                            fields = {'genres': {'state': 'empty'},
                                      'studios': {'state': 'empty', 'value': ['Hidden']}}
                            for lib in ('m1', 'm2'):
                                db.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?,?,'work',-1,-1,'zh-CN','input',?,?,'valid','nfo_v1',?,?,1)",
                                           (ids[lib], ids['movie'], 'root-' + lib,
                                            '/' + lib + '/movie.nfo', json.dumps(fields),
                                            json.dumps(['genres', 'studios'])))
                        else:
                            fields = {'genres': {'state': 'empty'}, 'studios': {'state': 'empty'}}
                            if scenario == 'display_studios':
                                fields['studios']['value'] = ['Hidden']
                            else:
                                fields['genres']['value'] = [{'source': 'tmdb', 'source_id': '18',
                                    'name': 'Hidden', 'filter_key': 'hidden'}]
                            db.execute('UPDATE media_metadata SET details_json=? WHERE media_id=? AND locale=?',
                                       (json.dumps({'version': 1, 'fields': fields}), ids['movie'], 'zh-CN'))
                    self.reseal(stage, baseline, path, manifest)
                    out = stage / 'malformed-empty.json'
                    result = self.invoke(baseline, path, out)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    category = 'genres' if scenario == 'display_genres' else 'studios'
                    for user in json.loads(out.read_text())['users'].values():
                        self.assertEqual(user[category]['status'], 'unverified')
                        self.assertTrue(user[category]['reason'])
                        self.assertEqual(user['counts']['status'], 'verified')

    def test_nfo_only_missing_studios_or_other_locale_adopted_row_is_unverified(self):
        for layout in ('old', 'new'):
            for state in ('missing', 'other_locale'):
                with self.subTest(layout=layout, state=state):
                    stage, baseline, path, manifest, ids = self.fixture(layout, label=layout + '-' + state)
                    with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                        db.execute('DELETE FROM media_metadata WHERE media_id=?', (ids['movie'],))
                        db.execute('DELETE FROM media_metadata_overrides WHERE media_id=?', (ids['movie'],))
                        fields = {'genres': {'state': 'empty'}}
                        if state == 'other_locale':
                            fields['studios'] = {'state': 'empty'}
                            db.execute('INSERT INTO media_metadata(media_id,locale,title,genres_json,details_json,revision) VALUES(?,?,?,?,?,1)',
                                       (ids['movie'], 'en-US', 'movie', '[]',
                                        json.dumps({'version': 1, 'fields': {'genres': {'state': 'empty'}}})))
                        for lib in ('m1', 'm2'):
                            db.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?,?,'work',-1,-1,'zh-CN','input',?,?,'valid','nfo_v1',?,?,1)",
                                       (ids[lib], ids['movie'], 'root-' + lib, '/' + lib + '/movie.nfo',
                                        json.dumps(fields), json.dumps(list(fields))))
                    self.reseal(stage, baseline, path, manifest)
                    out = stage / 'nfo-unknown.json'
                    result = self.invoke(baseline, path, out)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    for user in json.loads(out.read_text())['users'].values():
                        self.assertEqual(user['studios']['status'], 'unverified')
                        self.assertTrue(user['studios']['reason'])

    def test_admin_visibility_respects_admin_visible(self):
        stage, baseline, path, manifest, ids = self.fixture('new')
        with sqlite3.connect(baseline / 'data' / 'library.db') as db:
            db.execute('UPDATE libraries SET admin_visible=0 WHERE library_id=?', (ids['video'],))
        self.reseal(stage, baseline, path, manifest)
        out = stage / 'admin-hidden.json'
        result = self.invoke(baseline, path, out)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        users = json.loads(out.read_text())['users']
        self.assertEqual(users[ids['admin']]['counts']['value']['ItemCount'], 2)
        self.assertEqual(users[ids['alice']]['counts']['value']['ItemCount'], 3)

    def test_selected_acl_accepts_member_nonmember_and_admin(self):
        stage, baseline, path, manifest, ids = self.fixture('new')
        out = stage / 'selected-acl.json'
        result = self.invoke(baseline, path, out)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        users = json.loads(out.read_text())['users']
        self.assertIn(ids['m2'], users[ids['alice']]['movie_libraries'])
        self.assertNotIn(ids['m2'], users[ids['bob']]['movie_libraries'])
        self.assertIn(ids['m2'], users[ids['admin']]['movie_libraries'])

    def test_tool_digest_without_oracle_is_refused(self):
        stage, baseline, path, manifest, _ = self.fixture('new')
        with sqlite3.connect(baseline / 'data' / 'library.db') as db:
            db.execute("UPDATE libraries SET access_mode='everyone',member_ids_json='[]'")
        self.reseal(stage, baseline, path, manifest)
        manifest['tool_snapshot_sha256'] = old_tool_hash()
        path.write_bytes(canonical(manifest))
        out = stage / 'without-oracle.json'
        self.rejected(self.invoke(baseline, path, out))
        self.assertFalse(out.exists())

    def test_missing_provenance_and_wrong_commit_are_refused(self):
        stage, baseline, path, manifest, _ = self.fixture('new')
        del manifest['product']['binary_sha256']
        path.write_bytes(canonical(manifest))
        self.rejected(self.invoke(baseline, path, stage / 'missing.json'))
        manifest['product']['binary_sha256'] = BINARY_SHA
        path.write_bytes(canonical(manifest))
        self.rejected(self.invoke(baseline, path, stage / 'commit.json', 'c' * 40))
        self.assertFalse((stage / 'missing.json').exists())
        self.assertFalse((stage / 'commit.json').exists())

    def test_unsupported_schema_is_refused_and_frozen_lifecycle_is_unverified(self):
        stage, baseline, path, manifest, ids = self.fixture('new')
        with sqlite3.connect(baseline / 'data' / 'library.db') as db:
            db.execute('PRAGMA user_version=99')
        self.reseal(stage, baseline, path, manifest)
        self.rejected(self.invoke(baseline, path, stage / 'schema.json'))
        with sqlite3.connect(baseline / 'data' / 'library.db') as db:
            db.execute('PRAGMA user_version=2')
            db.execute("UPDATE libraries SET lifecycle_state='frozen' WHERE library_id=?", (ids['tv'],))
        self.reseal(stage, baseline, path, manifest)
        out = stage / 'frozen.json'
        result = self.invoke(baseline, path, out)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for user in json.loads(out.read_text())['users'].values():
            self.assertEqual(user['counts']['status'], 'unverified')
            self.assertTrue(user['counts']['reason'])

    def test_active_owner_and_wal_are_refused(self):
        stage, baseline, path, _, _ = self.fixture('new')
        (stage / 'owner.json').write_text('{}')
        self.rejected(self.invoke(baseline, path, stage / 'owner-output.json'))
        (stage / 'owner.json').unlink()
        (baseline / 'data' / 'library.db-wal').write_bytes(b'active')
        self.rejected(self.invoke(baseline, path, stage / 'wal-output.json'))

    def test_source_manifest_hashes_and_tool_hash_are_verified(self):
        stage, baseline, path, manifest, _ = self.fixture('new')
        for name in ('source_manifest', 'source_tree_manifest'):
            with self.subTest(name=name):
                source = Path(manifest['source_snapshot'][name + '_path'])
                original = source.read_bytes()
                source.write_bytes(original + b' ')
                out = stage / (name + '-stale.json')
                self.rejected(self.invoke(baseline, path, out))
                self.assertFalse(out.exists())
                source.write_bytes(original)
        manifest['tool_snapshot_sha256'] = '0' * 64
        path.write_bytes(canonical(manifest))
        out = stage / 'tool-stale.json'
        self.rejected(self.invoke(baseline, path, out))
        self.assertFalse(out.exists())

    def test_missing_source_and_tool_provenance_are_refused(self):
        stage, baseline, path, manifest, _ = self.fixture('new')
        for key in ('source_manifest_path', 'source_manifest_sha256',
                    'source_tree_manifest_path', 'source_tree_manifest_sha256'):
            with self.subTest(key=key):
                changed = json.loads(json.dumps(manifest))
                del changed['source_snapshot'][key]
                path.write_bytes(canonical(changed))
                out = stage / ('missing-' + key + '.json')
                self.rejected(self.invoke(baseline, path, out))
                self.assertFalse(out.exists())
        del manifest['tool_snapshot_sha256']
        path.write_bytes(canonical(manifest))
        self.rejected(self.invoke(baseline, path, stage / 'missing-tool.json'))

    def test_modified_schema_and_invalid_foreign_key_are_refused(self):
        for layout in ('old', 'new'):
            for mutation in ('schema', 'foreign_key', 'missing_media'):
                with self.subTest(layout=layout, mutation=mutation):
                    stage, baseline, path, manifest, ids = self.fixture(layout, label=layout + '-' + mutation)
                    with sqlite3.connect(baseline / 'data' / 'library.db') as db:
                        if mutation == 'schema':
                            db.execute('DROP INDEX files_presence')
                        elif mutation == 'foreign_key':
                            db.execute('PRAGMA foreign_keys=OFF')
                            db.execute("UPDATE library_files SET root_id='missing-root' WHERE file_id='a'")
                            self.assertTrue(db.execute('PRAGMA foreign_key_check').fetchall())
                        else:
                            db.execute('PRAGMA foreign_keys=OFF')
                            db.execute("UPDATE library_files SET media_id=? WHERE file_id='a'", (uid(layout, 999),))
                            self.assertTrue(db.execute('PRAGMA foreign_key_check').fetchall())
                    self.reseal(stage, baseline, path, manifest)
                    out = stage / 'invalid.json'
                    self.rejected(self.invoke(baseline, path, out))
                    self.assertFalse(out.exists())

    def test_resolved_output_inside_baseline_is_refused(self):
        stage, baseline, path, _, _ = self.fixture('new')
        alias = stage / 'alias'
        alias.symlink_to(baseline / 'data', target_is_directory=True)
        before = tree(baseline)
        out = alias / 'oracle-output.json'
        self.rejected(self.invoke(baseline, path, out))
        self.assertFalse(out.exists())
        self.assertEqual(tree(baseline), before)

    def test_shm_and_recent_database_owner_are_refused(self):
        stage, baseline, path, manifest, _ = self.fixture('new')
        sidecar = baseline / 'data' / 'library.db-shm'
        sidecar.write_bytes(b'active')
        self.reseal(stage, baseline, path, manifest)
        self.rejected(self.invoke(baseline, path, stage / 'shm.json'))
        sidecar.unlink()
        with sqlite3.connect(baseline / 'data' / 'app.db') as db:
            db.execute('INSERT INTO instance_lock(id,owner,heartbeat_at) VALUES(1,?,?)',
                       ('active-fixture', int(time.time() * 1000)))
        self.reseal(stage, baseline, path, manifest)
        self.rejected(self.invoke(baseline, path, stage / 'db-owner.json'))

    def test_existing_output_and_stale_source_are_refused(self):
        stage, baseline, path, manifest, _ = self.fixture('new')
        before = tree(baseline)
        out = stage / 'expectations.json'
        out.write_text('existing')
        result = self.invoke(baseline, path, out)
        self.rejected(result)
        self.assertEqual(out.read_text(), 'existing')
        self.assertEqual(tree(baseline), before)
        manifest['source_snapshot']['baseline_facts_sha256'] = '0' * 64
        path.write_bytes(canonical(manifest))
        fresh = stage / 'stale.json'
        result = self.invoke(baseline, path, fresh)
        self.rejected(result)
        self.assertFalse(fresh.exists())
        self.assertEqual(tree(baseline), before)

    def test_changed_stopped_source_is_refused(self):
        stage, baseline, path, _, _ = self.fixture('new')
        with sqlite3.connect(baseline / 'data' / 'app.db') as db:
            db.execute("UPDATE users SET enabled=0 WHERE login='bob'")
        changed = tree(baseline)
        out = stage / 'changed.json'
        result = self.invoke(baseline, path, out)
        self.rejected(result)
        self.assertFalse(out.exists())
        self.assertEqual(tree(baseline), changed)


if __name__ == '__main__':
    unittest.main()
