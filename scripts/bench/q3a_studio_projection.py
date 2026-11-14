"""Q3a: studio hydration — full JSON blobs + client parse vs SQL json_extract projection.

Synthetic 100k media, details_json ~3 KB, one work-scope source row each.
Measures one items page (200 ids) repeated over all pages, i.e. a full walk.
"""
import json, random, sqlite3, time, uuid

N, PAGE = 100_000, 200
db = sqlite3.connect(":memory:")
db.executescript("""
CREATE TABLE media_metadata(media_id TEXT, locale TEXT, details_json TEXT, PRIMARY KEY(media_id,locale));
CREATE TABLE media_metadata_sources(media_id TEXT, entity_scope TEXT, season INT, episode INT, source TEXT,
  locale TEXT, normalized_json TEXT, PRIMARY KEY(media_id,entity_scope,season,episode,source,locale));
""")
pad = "x" * 2800
ids = [str(uuid.uuid4()) for _ in range(N)]
rows, srcs = [], []
for i, m in enumerate(ids):
    d = {"overview": pad, "cast": [{"name": f"a{j}"} for j in range(20)]}
    if i % 2:
        d["studios"] = ["Studio %d" % (i % 50)]
    rows.append((m, "zh-CN", json.dumps(d)))
    srcs.append((m, "work", -1, -1, "tmdb", "zh-CN",
                 json.dumps({"fields": {"studios": ["S"], "studio_logo": {"value": "/l.png"}, "pad": pad}})))
db.executemany("INSERT INTO media_metadata VALUES(?,?,?)", rows)
db.executemany("INSERT INTO media_metadata_sources VALUES(?,?,?,?,?,?,?)", srcs)
JOIN = ("FROM media_metadata m LEFT JOIN media_metadata_sources s ON s.media_id=m.media_id "
        "AND s.entity_scope='work' AND s.season=-1 AND s.episode=-1 AND s.locale=m.locale "
        "WHERE m.media_id IN ({}) ORDER BY m.media_id, m.locale")
OLD = "SELECT m.media_id, m.locale, m.details_json, s.normalized_json " + JOIN
TXT = "CASE WHEN json_type({c},'{p}')='text' THEN json_extract({c},'{p}') END"
def first(c, p):
    return f"COALESCE({TXT.format(c=c, p=p)}, {TXT.format(c=c, p=p + '[0]')})"
NEW = ("SELECT m.media_id, m.locale, "
       f"COALESCE({TXT.format(c='m.details_json', p='$.studio')}, {first('m.details_json', '$.studios')}), "
       f"{TXT.format(c='m.details_json', p='$.studio_logo')}, "
       f"COALESCE({first('s.normalized_json', '$.fields.studios')}, {first('s.normalized_json', '$.studios')}), "
       f"COALESCE({TXT.format(c='s.normalized_json', p='$.fields.studio_logo.value')}, "
       f"{TXT.format(c='s.normalized_json', p='$.studio_logo.value')}) " + JOIN)
random.shuffle(ids)
def walk(sql, parse):
    t = time.perf_counter(); out = 0
    for i in range(0, N, PAGE):
        chunk = ids[i:i + PAGE]
        for r in db.execute(sql.format(",".join("?" * len(chunk))), chunk):
            out += parse(r)
    return (time.perf_counter() - t) * 1000, out
def parse_old(r):
    d = json.loads(r[2]); json.loads(r[3]) if r[3] else None
    return len(r[2]) + len(r[3] or "")
def parse_new(r):
    return sum(len(x or "") for x in r[2:])
for name, sql, p in [("old blobs+parse", OLD, parse_old), ("new json_extract", NEW, parse_new)]:
    ms, b = walk(sql, p)
    print(f"{name}: full walk {N} ids = {ms:.0f} ms, per 200-item page = {ms / (N / PAGE):.2f} ms, bytes to client = {b / 1e6:.1f} MB")
