import json,time,urllib.request,urllib.parse,sys,statistics
phase=sys.argv[1];N=int(sys.argv[2]) if len(sys.argv)>2 else 15
B='http://127.0.0.1:18765/api/v1';H={'Authorization':'Bearer changeme'}
def get(p,to=60):
    t=time.time()
    try:
        r=urllib.request.urlopen(urllib.request.Request(B+p,headers=H),timeout=to);b=r.read();c=r.status
    except urllib.error.HTTPError as e:b=e.read();c=e.code
    except Exception: b=b'';c=0
    return c,(time.time()-t)*1000,b
J=lambda p:json.loads(get(p)[2])['data']
libs={l['name']:l for l in J('/libraries')}
lib=libs['tv-en']['id'];its=J('/libraries/%s/items'%lib);it=its[0]
ledger=it['poster_url'].split('/')[-1]
jobs=J('/jobs');jl=jobs['items'] if isinstance(jobs,dict) else jobs
sub={'{id}':lib,'{item_id}':it['media_item_id'],'{ledger_id}':ledger,'{kind}':'tv','{index}':'0','{season}':'1'}
skip=['stream','/proxy','{operation_id}','{play_session_id}','/export','{collection_id}','/users/removals']
ext=['/discover','/media/','/search/torrents','/search/titles','/genres','/sites/boost','/sites/catalog','/subscriptions/automation']
special={'/media/{kind}/{id}':'/media/tv/1438','/media/tv/{id}/season/{season}':'/media/tv/1438/season/1','/media/person/{id}':'/media/person/31','/media/douban/{id}':'/media/douban/1292052','/jobs/{id}':'/jobs/'+str(jl[0]['id']),'/search/library-items':'/search/library-items?q=%E7%81%AB','/search/titles':'/search/titles?q=wire','/fs/browse':'/fs/browse?path=/tmp','/media/{id}/poster':'/media/'+it['media_item_id']+'/poster'}
res=[]
for l in open('/tmp/e2e/routes.txt'):
    ms,p=l.split();
    if 'get' not in ms or any(s in p for s in skip): continue
    u=special.get(p)
    if not u:
        u=p
        for k,v in sub.items(): u=u.replace(k,v)
        if '{' in u: continue
    n=3 if any(e in p for e in ext) else N
    ts=[];codes={}
    for i in range(n):
        c,ms_,_=get(u);ts.append(ms_);codes[c]=codes.get(c,0)+1
    ts.sort();res.append({'route':p,'n':n,'p50':round(statistics.median(ts),1),'p95':round(ts[min(n-1,int(n*0.95))],1),'max':round(ts[-1],1),'codes':codes})
    print(p,res[-1]['p50'],res[-1]['p95'],res[-1]['max'],codes,flush=True)
json.dump(res,open('/tmp/e2e/bench-%s.json'%phase,'w'),ensure_ascii=False)
