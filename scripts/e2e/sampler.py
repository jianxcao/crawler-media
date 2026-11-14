import json,time,urllib.request,sys
B='http://127.0.0.1:18765/api/v1';H={'Authorization':'Bearer changeme'}
def get(p,to=30):
    t=time.time()
    try:
        r=urllib.request.urlopen(urllib.request.Request(B+p,headers=H),timeout=to);b=r.read();c=r.status
    except urllib.error.HTTPError as e: b=e.read();c=e.code
    except Exception as e: b=b'';c=0
    return c,(time.time()-t)*1000,b
out=open('/tmp/e2e/samples.jsonl','a');pick={};last=0
while True:
    now=time.time()
    if now-last>30:
        last=now
        c,_,b=get('/libraries')
        try:
            for l in json.loads(b)['data']:
                if l['is_default']: continue
                c2,_,b2=get('/libraries/%s/items?limit=200'%l['id'])
                d=json.loads(b2)['data'];its=d['items'] if isinstance(d,dict) else d
                if its:
                    big=max(its,key=lambda i:i.get('episode_count') or 0)
                    pick[l['name']]=(l['id'],big['media_item_id'],big.get('title'))
                elif l['name'] in pick: del pick[l['name']]
        except Exception as e: pass
        c,_,b=get('/jobs?active_only=true')
        try: act=[j.get('kind') or j.get('name') for j in json.loads(b)['data']['items' ] ] if isinstance(json.loads(b)['data'],dict) else [j.get('kind') or j.get('name') for j in json.loads(b)['data']]
        except Exception: act=[]
        out.write(json.dumps({'t':now,'active':act})+'\n')
    for n,(lid,iid,tt) in list(pick.items()):
        c,ms,_=get('/libraries/%s/items/%s'%(lid,iid),60)
        out.write(json.dumps({'t':time.time(),'lib':n,'title':tt,'code':c,'ms':round(ms,1)})+'\n')
    out.flush();time.sleep(2)
