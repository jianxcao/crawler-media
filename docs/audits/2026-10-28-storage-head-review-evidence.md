# 7030c1dd 审查证据附录

本附录保存只读审查的临时探针源码与输出，不是产品实现或正式测试。所有写入只发生于探针自己的临时数据目录，Fetcher为离线fake；未访问用户实例。生产源码和测试保持不变。公开方法/真实Router与隔离SQL测量的边界见[审查报告](2026-10-28-storage-head-review.md)。

## 复核环境与构建

- HEAD：`7030c1ddc8b6e28e457fdd309d00bbc5a21c09d6`；main：`756444f76bef53f5fabed6d0ef53e9f280e005e9`。
- 当前workspace构建产物：`CARGO_TARGET_DIR=/tmp/crawler-review-20261028-target`，bundled SQLite 3.50.2，debug。
- 探针通过 `rustc --edition=2024 <source> -L dependency=<target>/debug/deps --extern <crate>=<matching-rlib> ... -o <probe>` 编译；api产物为 `libapi-7672cccbc5d1358b.rlib`，使用其fingerprint匹配的依赖，不混用不同构建版本的同名crate。
- 复核时可先运行本次报告的cargo命令生成当前产物，将下列代码提取到临时目录，按实际fingerprint选择rlib。路径常量按本地检出位置替换；scalar公共探针的SQL文件替换为下列SQL。
- 输出的UUID是每次临时生成的夹具ID；时间在workspace并发运行下只作参考，VM是同库原SQL的statement状态。

生产源文件SHA-256（供定位，不是对整个构建的可重现性保证）：

- `crates/store/src/fingerprint_models.rs`：`934da0f207d67f778f0c209b9b4073f90bc0536bb2ddd3666ef9ae68709ff558`
- `crates/api/src/probe_manager/season.rs`：`dcea72231ad044ecdc2f15bf857a54138ac1ac4293bc1915b1c6354e956f744a`
- `crates/api/src/http/library_duplicates.rs`：`948d7b70c89f7ee20981deb833357bcb2c425e076819fe4c4e54e0610367f5d6`
- `crates/store/src/library_query/series_scalar.rs`：`cccd1ff66e363afe97d8d4805fb45015e90cbba568cbc95b113f8c0942e2aeda`
- `crates/store/src/schema/v1/library.sql`：`9636f625223a42276d521f974571b2842db2e9044b37c09c5dce76dc6dedb290`
- `crates/api/src/http/rule_sets.rs`：`b43c31f0a57cd119510cee3e3304a67bf31805c5c66c293c3850014832d3b920`

## H1：跨阶段迟到模型与重开恢复

原临时源码：`/tmp/storage-current-model-probe.rs`。

```rust
use store::{Store,StoredFingerprintModel,StoredFingerprintModelMember};
use domain::{Media,MediaId,MediaKind};
use store::playback::MediaMergeRequest;
use std::collections::BTreeMap;
fn main(){
 let tmp=tempfile::tempdir().unwrap();let s=Store::open(tmp.path()).unwrap();
 let survivor=MediaId::new();let loser=MediaId::new();
 for id in [survivor,loser] {s.insert_media(&Media{id,kind:MediaKind::Tv,title:"test".into(),year:None,original_title:None,aliases:vec![],tmdb_id:None,douban_id:None,tvdb_id:None,bangumi_id:None,anilist_id:None}).unwrap();}
 let lib=s.default_library(MediaKind::Tv).unwrap().unwrap();
 let file=store::test_support::publish_fixture_file(&s,&store::test_support::FixtureFile{media_id:loser,path:lib.root_paths[0].join("late.mkv").display().to_string(),season:Some(1),episode:Some(1),..Default::default()}).unwrap();
 let op=s.begin_media_merge(&MediaMergeRequest{request_id:MediaId::new().to_string(),survivor,loser,survivor_revision:0,loser_revision:0,refresh_targets:BTreeMap::new()},1000).unwrap();
 let db=rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
 db.execute_batch("CREATE TRIGGER fail_retire BEFORE UPDATE OF phase_cursor ON media_operations WHEN NEW.phase_cursor='retire' BEGIN SELECT RAISE(ABORT,'boundary'); END;").unwrap();
 println!("first advance={:?}",s.advance_media_merge(&op,1100));
 println!("phase={}",s.media_operation_status(&op).unwrap().phase);
 let m=StoredFingerprintModel{model_id:"late".into(),media_id:loser.to_string(),season:1,kind:"intro".into(),model_version:1,membership_key:"key".into(),policy_key:"default".into(),model_json:"{}".into(),created_at_ms:1200};
 let current=s.file_coverage_by_id(file.file_id).unwrap().unwrap();
 println!("current file media is survivor={} source unchanged={}",current.file.media_id==survivor,current.file.version.source_version==file.version.source_version);
 let member=StoredFingerprintModelMember{model_id:"late".into(),sample_id:"sample".into(),ledger_id:file.file_id.to_string(),source_version:file.version.source_version.clone()};
 println!("late write={:?}",s.put_fingerprint_model(&m,&[member]));
 db.execute_batch("DROP TRIGGER fail_retire").unwrap();drop(db);drop(s);
 let s=Store::open(tmp.path()).unwrap();
 println!("resume after reopen={:?}",s.advance_media_merge(&op,1300));
 println!("final state={:?}",s.media_operation_status(&op));
}

```

实际输出：

```text
first advance=Err(Sqlite(SqliteFailure(Error { code: ConstraintViolation, extended_code: 1811 }, Some("boundary"))))
phase=subscribes
current file media is survivor=true source unchanged=true
late write=Ok(())
resume after reopen=Err(Invalid("cannot retire media 311c6796-d7da-4929-8d7b-afad5ee8f64e: references remain (fingerprint_season_models=1)"))
final state=Ok(OperationStatus { operation_id: "d22880d7-c5e1-4e90-a3f3-2f0b11e38bbd", phase: "retire", state: "failed", error: Some("invalid argument: cannot retire media 311c6796-d7da-4929-8d7b-afad5ee8f64e: references remain (fingerprint_season_models=1)") })
```

## H2：真实Router与独立持锁线程

原临时源码：`/tmp/storage-current-lock-probe.rs`。

```rust
//! Media operations and background artwork must keep a single Tokio worker free.

use std::sync::Arc;
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;


struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

// No runtime exists during construction, so unrelated background drivers stay off.
fn state_with_admin(root: &std::path::Path) -> (ApiState, String) {
    let state = ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "executor-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    let user = domain::User {
        id: domain::UserId::new(),
        login: "artwork-admin".into(),
        enabled: true,
        role: UserRole::Admin,
    };
    let token = "artwork-admin-session".to_string();
    {
        let handle = state.store();
        let store = handle.lock();
        store
            .insert_user_with_password(&user, "admin-pass")
            .unwrap();
        store.set_user_token(user.id, &token).unwrap();
    }
    (state, token)
}

fn request(method: &str, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn main() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let library_id = state.store().lock().list_libraries().unwrap()[0].id.clone();
    let rt = runtime();
    for uri in ["/api/v1/collections".to_string(), "/api/v1/libraries".to_string(), format!("/api/v1/libraries/{library_id}/duplicates")] {
        let app = router(state.clone());
        let store = state.store();
        let (tx,rx)=std::sync::mpsc::channel();
        let holder=std::thread::spawn(move || { let _guard=store.lock(); tx.send(()).unwrap(); std::thread::sleep(Duration::from_millis(500)); });
        rx.recv().unwrap();
        let (response,elapsed)=rt.block_on(async { let started=Instant::now(); tokio::join!(app.oneshot(request("GET",&uri,&token,Value::Null)), async {tokio::time::sleep(Duration::from_millis(10)).await;started.elapsed()}) });
        holder.join().unwrap();
        println!("{} status={} timer_ms={}",uri,response.unwrap().status(),elapsed.as_millis());
    }
}

```

实际输出：

```text
/api/v1/collections status=200 OK timer_ms=507
/api/v1/libraries status=200 OK timer_ms=506
/api/v1/libraries/c32a294a-32e7-411e-8a3e-7d8d17ef005b/duplicates status=200 OK timer_ms=502
```

## H3：公开Store scalar和同库原SQL

原临时源码：`/tmp/scalar_public.rs`。

```rust
extern crate store;
extern crate domain;
extern crate rusqlite;
use domain::{MediaKind, User, UserRole, UserId, MediaId, LibraryId, ProtocolParent};
use rusqlite::{Connection, params, StatementStatus};
fn main() {
    println!("sqlite {}", rusqlite::version());
    for n in [100,1000,5000] {
        let dir=std::env::temp_dir().join(format!("scalar-public-{}-{n}",std::process::id()));
        let store=store::Store::open(&dir).unwrap();
        let uid:UserId="00000000-0000-4000-8000-000000000001".parse().unwrap();
        let mid:MediaId="00000000-0000-4000-8000-000000000002".parse().unwrap();
        store.insert_user(&User{id:uid,login:"probe".into(),enabled:true,role:UserRole::Admin}).unwrap();
        let library=store.create_library(MediaKind::Tv,"Probe", &["/probe-root"], "everyone",true,&[]).unwrap();
        let lid:LibraryId=library.id.parse().unwrap();
        let root=store.list_library_roots().unwrap().into_iter().find(|r|r.library_id==library.id).unwrap().id;
        let c=Connection::open(dir.join("library.db")).unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON; BEGIN").unwrap();
        c.execute("INSERT INTO media(media_id,kind,title,identity_state) VALUES (?1,'tv','M','confirmed')",[mid.to_string()]).unwrap();
        for ep in 1..=n {
            let f=format!("00000000-0000-4000-9000-{ep:012}"); let path=format!("/probe-root/{ep}.mkv");
            c.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence) VALUES (?1,?2,?3,?4,'tv',?5,?5,'present','published',1,'s','probe','high')",params![f,library.id,root,mid.to_string(),path]).unwrap();
            c.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES (?1,?2,?3,1,?4)",params![f,library.id,mid.to_string(),ep]).unwrap();
            c.execute("INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms) VALUES (?1,?2,1,?3,1,1,100)",params![uid.to_string(),mid.to_string(),ep]).unwrap();
        }
        c.execute_batch("COMMIT").unwrap();
        for analyze in [false,true] {
            if analyze {c.execute_batch("ANALYZE").unwrap();}
            let t=std::time::Instant::now();let out=store.protocol_series_scalar(uid,mid,&ProtocolParent::Library(lid)).unwrap();
            assert_eq!(out.episodes,n as usize);assert_eq!(out.completed,n as usize);
            let public_ms=t.elapsed().as_millis();
            let sql=include_str!("/tmp/scalar.sql");
            let mut stmt=c.prepare(sql).unwrap();let result=stmt.query_row(params![uid.to_string(),mid.to_string(),lid.to_string(),lid.to_string()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(2)?))).unwrap();
            println!("n={n} analyze={analyze} public=({},{}) public_ms={public_ms} raw={result:?} vm={} fullscan={} sort={}",out.episodes,out.completed,stmt.get_status(StatementStatus::VmStep),stmt.get_status(StatementStatus::FullscanStep),stmt.get_status(StatementStatus::Sort));
        }
        drop(c);drop(store);std::fs::remove_dir_all(dir).unwrap();
    }
}

```

实际输出：

```text
sqlite 3.50.2
n=100 analyze=false public=(100,100) public_ms=2 raw=(100, 100) vm=169891 fullscan=297 sort=100
n=100 analyze=true public=(100,100) public_ms=3 raw=(100, 100) vm=14583 fullscan=495 sort=100
n=1000 analyze=false public=(1000,1000) public_ms=185 raw=(1000, 1000) vm=16098091 fullscan=2997 sort=1000
n=1000 analyze=true public=(1000,1000) public_ms=8 raw=(1000, 1000) vm=145083 fullscan=4995 sort=1000
n=5000 analyze=false public=(5000,5000) public_ms=4879 raw=(5000, 5000) vm=400490091 fullscan=14997 sort=5000
n=5000 analyze=true public=(5000,5000) public_ms=34 raw=(5000, 5000) vm=725083 fullscan=24995 sort=5000
```

测量SQL：生产`series_scalar_on`的Library scope、单个授权Library形式（?1 User、?2 Media、?3/?4 Library）。

```sql
WITH scoped_files AS (
               SELECT f.file_id, f.created_ns FROM library_files f
               WHERE f.media_id=?2 AND f.library_id IN (?3)
                 AND f.publication_state='published' AND f.presence_state='present'  AND f.library_id = ?4
             ), covered_files AS (
               SELECT f.file_id,f.created_ns FROM scoped_files f
               WHERE EXISTS (SELECT 1 FROM library_file_units u WHERE u.file_id=f.file_id
                             AND u.season>=0 AND u.episode>=0)
             ), owned AS (
               SELECT DISTINCT u.season, u.episode FROM library_file_units u
               JOIN covered_files f ON f.file_id=u.file_id
               WHERE u.season>=0 AND u.episode>=0
             ), marks AS (
               SELECT o.season,o.episode,
                 (SELECT p.played FROM playback_units p
                  WHERE p.user_id=?1 AND p.media_id=?2 AND
                    ((p.season=o.season AND p.episode=o.episode) OR
                     (p.season=o.season AND p.episode=-1) OR
                     (p.season=-1 AND p.episode=-1)) AND p.has_played_assertion=1
                  ORDER BY p.played_changed_at_ms DESC, p.season DESC, p.episode DESC
                  LIMIT 1) AS asserted,
                 (SELECT p.played FROM playback_units p WHERE p.user_id=?1 AND p.media_id=?2
                  AND p.season=o.season AND p.episode=o.episode) AS own
               FROM owned o)
             SELECT (SELECT COUNT(*) FROM owned),
                    (SELECT COUNT(DISTINCT season) FROM owned),
                    (SELECT COUNT(*) FROM marks WHERE COALESCE(asserted,own,0)=1),
                    (SELECT MAX(created_ns) FROM covered_files)
```

## H5：真实Router旧Filter输入

原临时源码：`/tmp/storage-review-alias-probe.rs`。

```rust
use std::sync::Arc;
use std::time::{Duration,Instant};
use api::{ApiState,Store};
use domain::UserRole;
use downloader::MemoryDownloader;
use indexer::{FetchRequest,Fetcher,IndexerError,ProfileSet};
use axum::body::{Body,to_bytes};
use axum::http::Request;
use tower::ServiceExt;
use serde_json::json;
struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

// No runtime exists during construction, so unrelated background drivers stay off.
fn state_with_admin(root: &std::path::Path) -> (ApiState, String) {
    let state = ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "executor-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap();
    let user = domain::User {
        id: domain::UserId::new(),
        login: "artwork-admin".into(),
        enabled: true,
        role: UserRole::Admin,
    };
    let token = "artwork-admin-session".to_string();
    {
        let handle = state.store();
        let store = handle.lock();
        store
            .insert_user_with_password(&user, "admin-pass")
            .unwrap();
        store.set_user_token(user.id, &token).unwrap();
    }
    (state, token)
}


fn main() {
 let tmp=tempfile::tempdir().unwrap();
 let (state,token)=state_with_admin(tmp.path());
 let app=api::router(state);
 let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
 runtime.block_on(async {
 for kind in ["platforms_block","hdr_block","release_group_block","hdr","no_such_kind"] {
  let body=json!({"name":format!("probe-{kind}"),"atoms":[{"kind":kind,"value":"DV","priority":100,"exclude":true}]});
  let req=Request::builder().method("POST").uri("/api/v1/rule-sets").header("content-type","application/json").header("authorization",format!("Bearer {token}")).body(Body::from(body.to_string())).unwrap();
  let res=app.clone().oneshot(req).await.unwrap();
  let status=res.status();
  let bytes=to_bytes(res.into_body(),usize::MAX).await.unwrap();
  println!("input={kind} status={status} body={}",String::from_utf8_lossy(&bytes));
 }
 });
}

```

实际输出：

```text
input=platforms_block status=201 Created body={"data":{"atoms":[{"exclude":true,"kind":"title_match","priority":100,"value":"DV"}],"id":"f5e1ef08-604e-4bc5-9531-e04ae9f95136","is_default":false,"keep_old_versions":false,"name":"probe-platforms_block"},"ok":true}
input=hdr_block status=201 Created body={"data":{"atoms":[{"exclude":true,"kind":"title_match","priority":100,"value":"DV"}],"id":"1c5cb9ca-bdc8-4a66-9355-bd969e2a0f43","is_default":false,"keep_old_versions":false,"name":"probe-hdr_block"},"ok":true}
input=release_group_block status=201 Created body={"data":{"atoms":[{"exclude":true,"kind":"title_match","priority":100,"value":"DV"}],"id":"6ab8e6f6-15c5-4727-9b1b-53a51d9967ee","is_default":false,"keep_old_versions":false,"name":"probe-release_group_block"},"ok":true}
input=hdr status=201 Created body={"data":{"atoms":[{"exclude":true,"kind":"hdr","priority":100,"value":"DV"}],"id":"b00f8c73-87f7-4626-836d-205397ecc052","is_default":false,"keep_old_versions":false,"name":"probe-hdr"},"ok":true}
input=no_such_kind status=400 Bad Request body={"error":{"code":"rule_set.invalid","message":"未知的 Filter atom no_such_kind"},"ok":false}
```

## 尚未测量

H4的二次比较与H6的覆盖复制由当前源码结构推导，本附录没有相应HTTP/RSS/分配数字。H1未运行真实指纹进程；H3未运行完整HTTP。独立设备、NAS、冷缓存、掉电和容量组合仍为Not run。
