import sys,json,urllib.request
full=sys.argv[2]=='full';phase=sys.argv[1]
H={'Authorization':'Bearer changeme'};B='http://127.0.0.1:18765/api/v1';F='http://127.0.0.1:3334'
g=lambda p:json.load(urllib.request.urlopen(urllib.request.Request(B+p,headers=H)))['data']
libs=[l for l in g('/libraries') if not l['is_default']]
urls=[('library',F+'/library')]
for l in libs:
    its=g('/libraries/%s/items?limit=200'%l['id'])
    if full: urls.append(('lib-'+l['name'],F+'/library/'+l['id']))
    for it in (its if full else sorted(its,key=lambda x:-(x.get('episode_count') or 0))[:1]):
        urls.append(('detail-%s-%s'%(l['name'],it['title']),'%s/library/%s/item/%s'%(F,l['id'],it['media_item_id'])))
if full:
    for p in ['/library/favorites','/library/manage','/library/customize','/settings','/tasks','/activity','/search','/collections','/subscriptions','/my','/discover/movie','/health']:
        urls.append(('page'+p.replace('/','-'),F+p))
json.dump({'phase':phase,'urls':urls},open('/tmp/e2e/urls-%s.json'%phase,'w'),ensure_ascii=False)
