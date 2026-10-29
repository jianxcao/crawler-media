import json,urllib.request,sys,collections
B='http://127.0.0.1:18765';H={'Authorization':'Bearer changeme'}
def raw(u):
    if u.startswith('/') and not u.startswith('/api'): u='/api/v1'+u
    url=u if u.startswith('http') else B+u
    try:
        r=urllib.request.urlopen(urllib.request.Request(url,headers=H),timeout=30);b=r.read(16);return r.status,b
    except urllib.error.HTTPError as e: return e.code,b''
    except Exception as e: return 0,b''
def img_ok(u):
    c,b=raw(u); return c==200 and (b[:3]==b'\xff\xd8\xff' or b[:8]==b'\x89PNG\r\n\x1a\n' or b[8:12]==b'WEBP' or b[:4] in (b'GIF8',)),c
J=lambda p:json.load(urllib.request.urlopen(urllib.request.Request(B+'/api/v1'+p,headers=H)))['data']
miss=collections.defaultdict(list);cnt=collections.Counter();bad=[]
for l in J('/libraries'):
    if l['is_default']: continue
    for it in J('/libraries/%s/items?limit=500'%l['id']):
        t=it['title'];d=J('/libraries/%s/items/%s'%(l['id'],it['media_item_id']))
        cnt['items']+=1
        for f in ['poster_url','backdrop_url','overview']:
            if not d.get(f): miss['item.'+f].append(t)
            elif f.endswith('url'):
                ok,c=img_ok(d[f]); cnt['img']+=1
                if not ok: bad.append((t,f,c))
        if not d.get('cast'): miss['item.cast'].append(t)
        for a in (d.get('cast') or [])[:5]:
            cnt['cast']+=1
            if not a.get('avatar_url'): miss['cast.avatar'].append('%s/%s'%(t,a['name']))
            else:
                ok,c=img_ok(a['avatar_url']);cnt['img']+=1
                if not ok: bad.append((t,'avatar '+a['name'],c))
        if not d.get('genres'): miss['item.genres'].append(t)
        cnt['seasons']+=len(d.get('seasons') or [])
        if it['kind']=='tv':
            eps=J('/libraries/%s/items/%s/episodes'%(l['id'],it['media_item_id']))
            for e in eps:
                cnt['episodes']+=1;n='%s S%sE%s'%(t,e['season_number'],e['episode_number'])
                if not e.get('overview'): miss['ep.overview'].append(n)
                if not e.get('name'): miss['ep.name'].append(n)
                if not e.get('still_url'): miss['ep.still'].append(n)
                else:
                    ok,c=img_ok(e['still_url']);cnt['img']+=1
                    if not ok: bad.append((n,'still',c))
print(json.dumps(cnt))
for k,v in miss.items(): print(k,len(v),v[:5])
print('bad images',len(bad),bad[:10])
if len(sys.argv)>1: json.dump({'cnt':cnt,'miss':miss,'bad':bad},open(sys.argv[1],'w'),ensure_ascii=False)
