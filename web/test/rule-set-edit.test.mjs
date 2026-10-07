import assert from 'node:assert/strict';
import test from 'node:test';
import { ruleSetEditAtoms } from '../lib/rule-set-edit.ts';

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
  const before = [atom('resolution','1080p',70),atom('resolution','720p',50)];
  const next = ruleSetEditAtoms(original,before,[...before,atom('resolution','2160p',100)]);
  assert.deepEqual(next.find(a=>a.value==='720p'),blocked);
  assert.equal(next.find(a=>a.value==='1080p').priority,321);
});
test('removing a condition removes only that dimension',()=>{
  assert.deepEqual(ruleSetEditAtoms([atom('hr',null,100,true)],[atom('hr',null,100,true)],[]),[]);
});
