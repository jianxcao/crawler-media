import sqlite3,sys
col,val=sys.argv[1],sys.argv[2]
for db in ['library','jobs','app']:
    c=sqlite3.connect('file:/Users/jianxcao/Downloads/crawler-media/data/%s.db?mode=ro'%db,uri=True)
    for (t,) in c.execute("select name from sqlite_master where type='table'"):
        cols=[r[1] for r in c.execute('pragma table_info("%s")'%t)]
        for cc in cols:
            if cc==col or (col=='media_id' and cc in('media_item_id',)) :
                n=c.execute('select count(*) from "%s" where "%s"=?'%(t,cc),(val,)).fetchone()[0]
                if n: print(db,t,cc,n)
