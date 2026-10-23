import sqlite3, time, os, uuid, sys
schema=open('crates/store/src/schema/v1/library.sql').read()
def run(with_idx):
    p='/tmp/q1.db'
    for s in ('','-wal'):
        try: os.remove(p+s)
        except FileNotFoundError: pass
    c=sqlite3.connect(p); c.executescript(schema)
    if not with_idx:
        for i in ('probe_stage_active_job','unidentified_root','media_survivor'): c.execute(f'DROP INDEX {i}')
    c.execute('PRAGMA foreign_keys=OFF')
    N=100000
    t=time.time(); c.execute('BEGIN')
    c.executemany("INSERT INTO probe_stage_state(file_id,context_key,stage,status,active_job_id,updated_at_ms) VALUES(?,?,?,?,?,0)",
      ((str(uuid.uuid4()),'c','s','running' if i%10==0 else 'done', str(uuid.uuid4()) if i%10==0 else None) for i in range(N)))
    c.executemany("INSERT INTO unidentified(library_id,root_id,path,confidence) VALUES(?,?,?,?)",
      (('L',f'r{i%20}',f'/p/{i}','low') for i in range(N)))
    c.execute('COMMIT'); ins=time.time()-t
    c.execute('VACUUM')
    pages=c.execute('PRAGMA page_count').fetchone()[0]*c.execute('PRAGMA page_size').fetchone()[0]
    t=time.time()
    for i in range(200): c.execute("SELECT 1 FROM probe_stage_state WHERE active_job_id=?",(str(i),)).fetchall(); c.execute("SELECT 1 FROM unidentified WHERE root_id=?",('rX',)).fetchall()
    q=time.time()-t
    return ins,pages,q
for w in (False,True):
    ins,size,q=run(w); print(f"indexes={w}: insert 200k rows {ins*1000:.0f} ms, db {size/1e6:.2f} MB, 400 lookups {q*1000:.1f} ms")
