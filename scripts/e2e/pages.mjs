const fs=await import("node:fs");
const cfg=JSON.parse(fs.readFileSync("/tmp/e2e/cur.json","utf8"));
const task=await taskSpace("crawler-media-e2e-r2");const page=task.page("p1");
const SH="/Users/jianxcao/work/fun/crawler-media/docs/test-reports/2026-10-29-e2e-r2/";
await page.goto("http://127.0.0.1:3334/library");
if((await page.url()).includes("/login")){
  await page.fill("input[type=text]","admin");await page.fill("input[type=password]","test-admin-secret-password");
  await page.click("button[type=submit]");await page.waitForURL(u=>!String(u).includes("/login"),{timeout:15000}).catch(()=>{});
}
const out=[];let i=0;
for(const [n,u] of cfg.urls){
  const t0=Date.now();
  try{await page.goto(u);}catch(e){}
  await page.waitForFunction(()=>(document.querySelector('h1,h2')||0).innerText&&!document.querySelector('.animate-pulse'),undefined,{timeout:20000}).catch(()=>{});
  const wall=Date.now()-t0;
  await page.waitForTimeout(800);
  const d=await page.evaluate(async()=>{
    const r=performance.getEntriesByType('resource').filter(e=>e.initiatorType==='fetch'||e.initiatorType==='xmlhttprequest').map(e=>({u:e.name.replace(location.origin,'').slice(0,100),ms:Math.round(e.duration)}));
    const im=[...document.images];
    return {url:location.pathname,api:r,imgs:im.length,broken:im.filter(i=>i.complete&&i.naturalWidth===0).map(i=>i.currentSrc.slice(0,120)),pending:im.filter(i=>!i.complete).length,text:document.body.innerText.slice(0,300)};
  }).catch(e=>({err:String(e)}));
  const fn=SH+cfg.phase+"-"+String(i++).padStart(2,"0")+"-"+n.replace(/[\/ ]/g,"_")+".png";
  await page.screenshot({path:fn}).catch(()=>{});
  Object.assign(d,{name:n,wall,phase:cfg.phase,t:Date.now()});out.push(d);
  console.log(n,wall,Math.max(0,...(d.api||[]).map(a=>a.ms)),"imgs",d.imgs,"broken",(d.broken||[]).length);
}
fs.writeFileSync("/tmp/e2e/pages-"+cfg.phase+".json",JSON.stringify(out,null,1));
