#!/usr/bin/env python3
"""生成压测媒体：python3 gen-stress.py <out_dir> [movies=3000] [shows=300] [eps=70]
strm 指向 http://127.0.0.1:9（不可达，探测快速失败，不产生外网流量），带 NFO（不需要 TMDB）。"""
import os,sys,random
out=sys.argv[1];NM=int(sys.argv[2]) if len(sys.argv)>2 else 3000;NS=int(sys.argv[3]) if len(sys.argv)>3 else 300;NE=int(sys.argv[4]) if len(sys.argv)>4 else 70
G=['剧情','喜剧','动作','爱情','科幻','动画','悬疑','惊悚','纪录','家庭'];r=random.Random(1)
act=lambda:''.join(f'<actor><name>演员{r.randrange(400):03d}</name><role>角色{i}</role></actor>' for i in range(6))
gen=lambda:''.join(f'<genre>{g}</genre>' for g in r.sample(G,2))
for i in range(NM):
    y=1980+i%45;n=f'压测电影{i:04d} ({y})';d=f'{out}/movie/{n}';os.makedirs(d,exist_ok=True)
    open(f'{d}/{n} - 1080p.strm','w').write(f'http://127.0.0.1:9/m/{i}.mkv\n')
    open(f'{d}/{n} - 1080p.nfo','w').write(f'<?xml version="1.0"?><movie><title>压测电影{i:04d}</title><year>{y}</year><plot>简介{i}</plot><uniqueid type="tmdb" default="true">{9000000+i}</uniqueid>{gen()}{act()}</movie>')
for s in range(NS):
    y=1990+s%35;n=f'压测剧集{s:03d} ({y})';d=f'{out}/tv/{n}';os.makedirs(f'{d}/Season 1',exist_ok=True)
    open(f'{d}/tvshow.nfo','w').write(f'<?xml version="1.0"?><tvshow><title>压测剧集{s:03d}</title><year>{y}</year><plot>剧集简介{s}</plot><uniqueid type="tmdb" default="true">{9500000+s}</uniqueid>{gen()}{act()}</tvshow>')
    for e in range(1,NE+1):
        open(f'{d}/Season 1/压测剧集{s:03d} S01E{e:02d}.strm','w').write(f'http://127.0.0.1:9/t/{s}/{e}.mkv\n')
print('done',NM,NS*NE)
