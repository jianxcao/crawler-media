const task=await taskSpace("crawler-media-e2e-r2");const page=task.page("p1");
await page.goto("http://127.0.0.1:3334/library");await page.waitForTimeout(1500);
await page.cdp("Network.enable",{});await page.events();
const URLS=["http://127.0.0.1:3334/library", "http://127.0.0.1:3334/library/4eb39864-e90d-418d-aca3-d80cc9dbfecb/item/61bff9f2-9128-440e-9fd1-5de05a1ebf08", "http://127.0.0.1:3334/library/9d752076-788b-45f7-bce9-d89f649715f7/item/98984a65-f8ad-4dc3-82a6-60944df6d050", "http://127.0.0.1:3334/library/90b46160-2a2f-47a8-ad7d-bae8e00ca0b4/item/00682088-a0d3-4438-97eb-69c16f5c34ef", "http://127.0.0.1:3334/library/1f35896f-3c9c-4d4b-aa99-1b7d760f35c4/item/f79b7f1e-9ed7-482a-89ea-2d278f46bcb4", "http://127.0.0.1:3334/library/305332aa-7c06-48c7-972d-f2c7a3718c86/item/29245026-4ec1-4c55-9fce-dabad58e52b6", "http://127.0.0.1:3334/library/fd193e0d-c9e2-4fed-a71d-7b4c63f200ed/item/738af674-0f09-4e84-b2a9-326fedd5e5cb", "http://127.0.0.1:3334/library/21dae1a9-ac37-492d-a69d-9f2315d2adbe/item/8edd456e-7d4d-40b5-812b-7f1fa817fdce", "http://127.0.0.1:3334/library/102e862f-db18-4dc1-b057-a0240698daf7/item/8c75f81c-fb9a-4110-a57f-3a6dabcf1baa", "http://127.0.0.1:3334/library/df5cf22d-380e-4307-807b-faa8a1bb8e4d/item/86378921-bae0-44b5-8941-828ba5ad05c2"];
for(let k=0;k<3;k++)for(const U of URLS){await page.events();
const t0=Date.now();await page.goto(U);
await page.waitForFunction(()=>document.body.innerText.includes('工作台加载失败')||(document.querySelector('h1')||0).innerText,undefined,{timeout:20000}).catch(()=>{});
console.log("t",Date.now()-t0, (await page.evaluate(()=>document.body.innerText.slice(0,40))).replace(/\n/g," "));
const bad=(Date.now()-t0)>5000;if(!bad)continue;
const ev=await page.events();const req={};
for(const e of ev){const m=e.method||e.name;const p=e.params||e.data||{};
 if(m==="Network.requestWillBeSent")req[p.requestId]={u:p.request.url.slice(0,110),t:p.timestamp,type:p.type};
 if(m==="Network.responseReceived"&&req[p.requestId])req[p.requestId].st=p.response.status,req[p.requestId].proto=p.response.protocol,req[p.requestId].tr=p.timestamp;
 if(m==="Network.loadingFinished"&&req[p.requestId])req[p.requestId].done=p.timestamp;
 if(m==="Network.loadingFailed"&&req[p.requestId])req[p.requestId].fail=p.errorText;}
const arr=Object.values(req);const t=Math.min(...arr.map(r=>r.t));
console.log("events",ev.length,"reqs",arr.length);
for(const r of arr){ if(!r.done||r.fail||(r.done-r.t)>1||r.u.includes('/auth'))console.log(((r.t-t)*1000|0),r.done?((r.done-r.t)*1000|0):'PENDING',r.st,r.proto,r.type,r.fail||'',r.u);}

break;}