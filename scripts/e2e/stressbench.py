import json,time,urllib.request,urllib.parse,random,statistics,sys
B='http://127.0.0.1:18766/api/v1';H={'Authorization':'Bearer changeme'}
def get(p):
    t=time.time()
    try: r=urllib.request.urlopen(urllib.request.Request(B+p,headers=H),timeout=60);b=r.read();c=r.status
    except urllib.error.HTTPError as e: b=e.read();c=e.code
    return c,(time.time()-t)*1000,b
libs={l['name']:l['id'] for l in json.loads(get('/libraries')[2])['data']}
M,T=libs['stress-movie'],libs['stress-tv']
def allids(L):
    ids=[];cur=None
    while True:
        p='/libraries/%s/items?limit=500'%L+('&cursor='+urllib.parse.quote(cur) if cur else '')
        c,ms,b=get(p);d=json.loads(b);ids+= [i['media_item_id'] for i in d['data']]
        cur=d.get('next_cursor') or (d.get('meta') or {}).get('next_cursor')
        if not cur or not d['data']: break
    return ids
mi=allids(M);ti=allids(T);print('ids',len(mi),len(ti))
gen=[g['value'] for g in json.loads(get('/libraries/%s/facets'%M)[2])['data']['genres']]
q=lambda s:urllib.parse.quote(s)
cases={
 'detail-movie':lambda:'/libraries/%s/items/%s'%(M,random.choice(mi)),
 'detail-tv':lambda:'/libraries/%s/items/%s'%(T,random.choice(ti)),
 'episodes-tv(70)':lambda:'/libraries/%s/items/%s/episodes'%(T,random.choice(ti)),
 'list-movie-50':lambda:'/libraries/%s/items?limit=50'%M,
 'list-movie-sort-title':lambda:'/libraries/%s/items?limit=50&sort=title&order=asc'%M,
 'list-movie-sort-rating':lambda:'/libraries/%s/items?limit=50&sort=rating'%M,
 'list-tv-50':lambda:'/libraries/%s/items?limit=50'%T,
 'list-movie-genre':lambda:'/libraries/%s/items?limit=50&g=%s'%(M,random.choice(gen)),
 'list-movie-decade':lambda:'/libraries/%s/items?limit=50&d=1990s'%M,
 'facets-movie':lambda:'/libraries/%s/facets'%M,
 'facets-tv':lambda:'/libraries/%s/facets'%T,
 'search-title':lambda:'/search/library-items?keyword='+q('压测电影%04d'%random.randrange(3000)),
 'search-prefix':lambda:'/search/library-items?keyword='+q('压测'),
 'search-person':lambda:'/search/library-items?keyword='+q('演员%03d'%random.randrange(400)),
 'item-index':lambda:'/libraries/%s/item-index'%M,
 'gallery':lambda:'/libraries/%s/gallery?limit=50'%M,
 'libraries':lambda:'/libraries',
}
N=int(sys.argv[1]) if len(sys.argv)>1 else 40;res={}
for k,f in cases.items():
    ts=[];codes={}
    for i in range(N):
        c,ms,b=get(f());ts.append(ms);codes[c]=codes.get(c,0)+1
    ts.sort();res[k]=dict(n=N,p50=round(statistics.median(ts),1),p95=round(ts[int(N*.95)],1),max=round(ts[-1],1),codes=codes,bytes=len(b))
    print('%-24s p50=%7.1f p95=%7.1f max=%7.1f %s %dB'%(k,res[k]['p50'],res[k]['p95'],res[k]['max'],codes,len(b)),flush=True)
json.dump(res,open('/tmp/e2e/stress-%s.json'%(sys.argv[2] if len(sys.argv)>2 else 'r'),'w'),ensure_ascii=False)
