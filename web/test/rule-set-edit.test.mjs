import assert from 'node:assert/strict';
import test from 'node:test';
import { ruleSetEditAtoms, ruleAtomValues } from '../lib/rule-set-edit.ts';

const atom = (kind, value, priority = 50, exclude = false) => ({kind,value,priority,exclude});
test('rename preserves exact stored priority, exclusion and opaque atoms', () => {
  const original = [atom('resolution','1080p',321),atom('custom','opaque',99,true)];
  const projection = [atom('resolution','1080p',70)];
  assert.deepEqual(ruleSetEditAtoms(original,projection,projection),original);
});
for (const [kind, from, to] of [
  ['resolution','1080p','2160p'], ['min_seeders','1','10'], ['video_codec','avc','hevc'],
]) {
  test(`editing ${kind} changes its value while preserving other atoms`, () => {
    const opaque = atom('custom','opaque',99,true);
    const next = ruleSetEditAtoms([atom(kind,from),opaque],[atom(kind,from)],[atom(kind,to)]);
    assert.equal(next.find(a=>a.kind===kind).value,to);
    assert.deepEqual(next.find(a=>a.kind==='custom'),opaque);
  });
}
test('rename preserves raw codec despite editor family expansion', () => {
  const stored = [atom('video_codec','HEVC',321,true)];
  const initial = ['x265','H.265','HEVC'].map(v=>atom('video_codec',v,90));
  assert.deepEqual(ruleSetEditAtoms(stored,initial,initial),stored);
});
test('editing HDR allow list preserves unchanged blacklist metadata',()=>{
  const blocked = atom('hdr','DV',987,true);
  const original = [atom('hdr','HDR10',321),blocked];
  const next = ruleSetEditAtoms(original,[atom('hdr','HDR10'),atom('hdr','DV',100,true)],
    [atom('hdr','HDR10+'),atom('hdr','DV',100,true)]);
  assert.deepEqual(next.find(a=>a.value==='DV'),blocked);
});
test('adding a resolution preserves an unchanged excluded sibling and its priority',()=>{
  const blocked = atom('resolution','720p',987,true);
  const original = [atom('resolution','1080p',321),blocked];
  const before = [atom('resolution','1080p',70),atom('resolution','720p',50,true)];
  const next = ruleSetEditAtoms(original,before,[...before,atom('resolution','2160p',100)]);
  assert.deepEqual(next.find(a=>a.value==='720p'),blocked);
  assert.equal(next.find(a=>a.value==='1080p').priority,70);
  assert.deepEqual(ruleAtomValues(next, 'resolution'), ['2160p', '1080p']);
});
test('removing a condition removes only that dimension',()=>{
  assert.deepEqual(ruleSetEditAtoms([atom('hr',null,100,true)],[atom('hr',null,100,true)],[]),[]);
});

test('strict_hr_toggle_survives_save_and_reload', () => {
  const original = [
    { ...atom('hr', null, 321, true), unknown: 'allow' },
    atom('platform', 'Netflix', 234),
    atom('platform', 'Disney+', 123, true),
    atom('team', 'Group-A', 456),
    atom('team', 'Group-B', 789, true),
  ];
  const edited = original.map((condition) => condition.kind === 'hr'
    ? { ...condition, unknown: 'exclude' }
    : condition);
  const saved = JSON.parse(JSON.stringify({
    atoms: ruleSetEditAtoms(original, original, edited),
  }));
  assert.equal(saved.atoms.find((condition) => condition.kind === 'hr').unknown, 'exclude');
  assert.deepEqual(saved.atoms.filter((condition) => condition.kind !== 'hr'), original.slice(1));
  const reloaded = ruleSetEditAtoms(saved.atoms, saved.atoms, saved.atoms);
  assert.deepEqual(reloaded, saved.atoms);
  const relaxed = reloaded.map((condition) => condition.kind === 'hr'
    ? { ...condition, unknown: 'allow' }
    : condition);
  assert.equal(ruleSetEditAtoms(reloaded, reloaded, relaxed)
    .find((condition) => condition.kind === 'hr').unknown, 'allow');
});

test('editing visible resolution preserves excluded atoms outside the controls', () => {
  const blocked = atom('resolution', '480p', 987, true);
  const next = ruleSetEditAtoms(
    [atom('resolution', '1080p'), blocked],
    [atom('resolution', '1080p')],
    [atom('resolution', '2160p')],
  );
  assert.deepEqual(next.find((condition) => condition.value === '480p'), blocked);
});

test('reordering preferred sites updates the persisted ranking', () => {
  const original = [atom('site', 'a', 100), atom('site', 'b', 50)];
  const reordered = [atom('site', 'b', 100), atom('site', 'a', 50)];
  assert.deepEqual(ruleSetEditAtoms(original, original, reordered), reordered);
});

test('editing a whitelist preserves a hidden blacklist with the same value', () => {
  const blocked = atom('resolution', '1080p', 987, true);
  const next = ruleSetEditAtoms(
    [atom('resolution', '1080p'), blocked],
    [atom('resolution', '1080p')],
    [atom('resolution', '2160p')],
  );
  assert.deepEqual(next.find((condition) => condition.exclude), blocked);
});


for (const [kind, first, second, inserted] of [
  ['resolution', '2160p', '1080p', '4320p'],
  ['source', 'remux', 'web-dl', 'blu-ray'],
  ['hdr', 'DV', 'HDR10', 'HDR10+'],
  ['platform', 'netflix', 'amazon', 'disney_plus'],
]) {
  test(`${kind} edited preference order survives save and reload`, () => {
    const blocked = atom(kind, 'blocked', 987, true);
    const untouched = atom('team', 'Group-A', 654);
    const original = [atom(kind, first, 321), atom(kind, second, 123), blocked, untouched];
    const before = [atom(kind, first, 100), atom(kind, second, 50), atom(kind, 'blocked', 50, true)];
    const after = [atom(kind, second, 100), atom(kind, first, 50), atom(kind, 'blocked', 50, true)];
    const reloaded = JSON.parse(JSON.stringify(ruleSetEditAtoms(original, before, after)));
    assert.deepEqual(ruleAtomValues(reloaded, kind), [second, first]);
    assert.deepEqual(reloaded.find((condition) => condition.exclude), blocked);
    assert.deepEqual(reloaded.find((condition) => condition.kind === 'team'), untouched);
  });

  test(`${kind} new preferred value outranks arbitrary stored priority after reload`, () => {
    const blocked = atom(kind, 'blocked', 987, true);
    const original = [atom(kind, second, 321), blocked];
    const before = [atom(kind, second, 50)];
    const after = [atom(kind, inserted, 100), atom(kind, second, 50)];
    const reloaded = JSON.parse(JSON.stringify(ruleSetEditAtoms(original, before, after)));
    assert.deepEqual(ruleAtomValues(reloaded, kind), [inserted, second]);
    assert.deepEqual(reloaded.find((condition) => condition.exclude), blocked);
  });
}
