import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

async function loadTs(path,dependencies={}) {
  const source=await readFile(new URL(path,import.meta.url),"utf8");
  const js=ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
  const module={exports:{}};
  new Function("require","module","exports",js)((id)=>dependencies[id]??{},module,module.exports);
  return module.exports;
}
async function client(request) {
  return loadTs("../lib/api/playback.ts",{
    "@/lib/http":{request,resolveRequestUrl:(path)=>path},
    "@/lib/env":{publicEnv:{}},
  });
}
const page=(ids,total=null,next_cursor=null)=>({items:ids.map(media_item_id=>({media_item_id})),total,next_cursor});

test("favorite page client serializes cursor and preserves nullable total",async()=>{
  const requests=[];
  const expected=page(["m"],null,"next");
  const api=await client(async path=>{requests.push(path);return{ok:true,data:expected};});
  const actual=await api.listFavorites({limit:60,after:"opaque+/=",sort:"title",order:"desc",unwatched_first:true});
  assert.deepEqual(actual,expected);
  const url=new URL(requests[0],"http://local");
  assert.equal(url.searchParams.get("limit"),"60");
  assert.equal(url.searchParams.get("after"),"opaque+/=");
  assert.equal(url.searchParams.get("unwatched_first"),"true");
  assert.equal(url.searchParams.get("sort"),"title");
  assert.equal(url.searchParams.get("order"),"desc");
  assert.equal(url.searchParams.has("offset"),false);
});

test("favorite gallery client retains selected-Media page metadata",async()=>{
  const requests=[];
  const expected={items:[{media_item_id:"empty",images:[]}],total:25,next_cursor:"tail"};
  const api=await client(async path=>{requests.push(path);return{ok:true,data:expected};});
  assert.deepEqual(await api.listFavoritesGallery({limit:10,after:"same",sort:"year",order:"asc"}),expected);
  const url=new URL(requests[0],"http://local");
  assert.equal(url.pathname,"/playback/favorites/gallery");
  assert.equal(url.searchParams.get("after"),"same");
  assert.equal(url.searchParams.has("offset"),false);
});

test("favorite position client requests an explicit scoped boundary",async()=>{
  const requests=[];
  const api=await client(async path=>{requests.push(path);return{ok:true,data:{start:300,after:"boundary"}};});
  assert.deepEqual(await api.seekFavoritesPosition({start:300,sort:"title",order:"asc"}),{start:300,after:"boundary"});
  const url=new URL(requests[0],"http://local");
  assert.equal(url.pathname,"/playback/favorites/position");
  assert.equal(url.searchParams.get("start"),"300");
  assert.equal(url.searchParams.get("sort"),"title");
  assert.equal(url.searchParams.has("offset"),false);
});

async function windowModel(load,seek) {
  const { FavoriteWindow }=await loadTs("../lib/favorite-window.ts");
  return new FavoriteWindow(load,seek);
}
const scope={sort:"title",order:"asc"};

test("favorite window appends through cached cursor, while prepend and recall seek positions",async()=>{
  const requests=[];
  const positions=[];
  const window=await windowModel(async options=>{
    requests.push(options);
    if(!options.after)return page(["0","1"],8,"after2");
    if(options.after==="after2")return page(["2","3"],null,"after4");
    return page(["6","7"],null,null);
  },async options=>{positions.push(options.start);return{start:options.start,after:`after${options.start}`};});
  await window.page(0,2,scope);
  await window.page(2,2,scope);
  assert.deepEqual(positions,[],"ordinary append must reuse server cursor");
  assert.equal(requests[1].after,"after2");
  await window.page(6,2,scope);
  assert.deepEqual(positions,[6]);
  assert.equal(requests[2].after,"after6");
  assert.ok(requests.every(options=>!("offset"in options)));
});

test("favorite restored-window refresh counts head once then follows cursor sequentially",async()=>{
  const requests=[];
  let outstanding=0;
  const window=await windowModel(async options=>{
    outstanding+=1;
    assert.equal(outstanding,1,"cursor requests depend on previous response");
    requests.push(options);
    await Promise.resolve();
    outstanding-=1;
    if(!options.after)return page(["0","1"],8,"after2");
    if(options.after==="after4")return page(["4","5"],null,"after6");
    return page(["6","7"],null,null);
  },async options=>({start:options.start,after:`after${options.start}`}));
  const result=await window.refresh(4,4,2,scope);
  assert.equal(result.total,8);
  assert.equal(result.start,4);
  assert.deepEqual(result.items.map(row=>row.media_item_id),["4","5","6","7"]);
  assert.equal(result.next_cursor,null);
  assert.equal(requests.filter(options=>!options.after).length,1);
  assert.deepEqual(requests.map(options=>options.after??null),[null,"after4","after6"]);
});

test("favorite window refresh beyond a shortened end returns head with exact count",async()=>{
  const window=await windowModel(async options=>!options.after?page(["0","1"],2,null):page([],null,null),async options=>{throw new Error(`out of range ${options.start}`);});
  const result=await window.refresh(100,60,60,scope);
  assert.equal(result.start,0);
  assert.equal(result.total,2);
  assert.deepEqual(result.items.map(row=>row.media_item_id),["0","1"]);
});

test("favorite supported-sort options match server capabilities and persisted unknown choice has explicit default",async()=>{
  const api=await client(async()=>({ok:true,data:page([],0)}));
  assert.deepEqual(api.FAVORITE_SORTS,["favorited_at","title","year"]);
  const home=await loadTs("../lib/home-rows.ts");
  assert.deepEqual(Object.keys(home.FAVORITES_SORT_PRESETS).sort(),["favorited_at","title","unwatched_first","year"].sort());
  const rows=home.buildHomeRows({rows:[{id:"favorites",sort:"rating"}]},[],[]);
  assert.equal(rows.find(row=>row.kind==="favorites").sort,"unwatched_first");
});

test("favorite window ignores an outstanding tail after same-scope jump invalidation", async () => {
  let resolveTail;
  const pendingTail = new Promise((resolve) => { resolveTail = resolve; });
  const model = await windowModel(async (options) => {
    if (!options.after) return page(["0", "1"], 8, "after2");
    if (options.after === "after2") return pendingTail;
    return page(["6", "7"], null, null);
  }, async (options) => ({ start: options.start, after: `after${options.start}` }));
  await model.page(0, 2, scope);
  const oldTail = model.page(2, 2, scope);
  const rejected = assert.rejects(oldTail, { name: "Error", message: "Favorite window replaced" });
  model.invalidate();
  const jumped = await model.page(6, 2, scope);
  assert.deepEqual(jumped.items.map((item) => item.media_item_id), ["6", "7"]);
  resolveTail(page(["2", "3"], null, "after4"));
  await rejected;
});

test("favorite window retries a failed append with the same known boundary", async () => {
  const requests = [];
  let fail = true;
  const model = await windowModel(async (options) => {
    requests.push(options.after ?? null);
    if (!options.after) return page(["0", "1"], 4, "after2");
    if (fail) { fail = false; throw new Error("temporary page failure"); }
    return page(["2", "3"], null, null);
  }, async () => { throw new Error("append must not seek"); });
  await model.page(0, 2, scope);
  await assert.rejects(model.page(2, 2, scope), /temporary page failure/);
  const retried = await model.page(2, 2, scope);
  assert.deepEqual(retried.items.map((item) => item.media_item_id), ["2", "3"]);
  assert.deepEqual(requests, [null, "after2", "after2"]);
});

test("favorite window resets cached boundaries when sort scope changes", async () => {
  const positions = [];
  const model = await windowModel(async (options) => page(["a", "b"], options.after ? null : 4, "old-tail"),
    async (options) => { positions.push(options); return { start: options.start, after: "new-scope-boundary" }; });
  await model.page(0, 2, scope);
  await model.page(2, 2, { sort: "year", order: "desc" });
  assert.deepEqual(positions, [{ start: 2, sort: "year", order: "desc" }]);
});


test("favorite wall failure stays retryable after a concurrent gallery succeeds", async () => {
  const { favoriteFailures, initialFavoriteFailures } = await loadTs("../lib/favorite-failures.ts");
  let state = favoriteFailures(initialFavoriteFailures, { channel: "wall", failed: true });
  state = favoriteFailures(state, { channel: "gallery", failed: false });
  assert.deepEqual(state, { wall: true, gallery: false });
  state = favoriteFailures(state, { channel: "wall", failed: false });
  assert.deepEqual(state, { wall: false, gallery: false });
});

test("favorite gallery failure stays retryable after a concurrent wall succeeds", async () => {
  const { favoriteFailures, initialFavoriteFailures } = await loadTs("../lib/favorite-failures.ts");
  let state = favoriteFailures(initialFavoriteFailures, { channel: "gallery", failed: true });
  state = favoriteFailures(state, { channel: "wall", failed: false });
  assert.deepEqual(state, { wall: false, gallery: true });
  state = favoriteFailures(state, { channel: "gallery", failed: false });
  assert.deepEqual(state, { wall: false, gallery: false });
});
