use domain::{DownloaderId, SubscribeId};
use store::download_operations::DownloadOwnershipState;
use store::plugins::PluginError;
use store::{Store, StoreError};

use super::{accept_input, insert_subscribe, own_input};

#[test]
fn download_ownership_rejects_cross_op_ownerless_delete_while_allowing_same_op_and_matching_owner()
{
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (dl_id, sub_a) = (DownloaderId::new(), SubscribeId::new());
    insert_subscribe(&store, sub_a, None);
    let (owned, del) = (
        DownloadOwnershipState::Owned,
        DownloadOwnershipState::Deleted,
    );
    let pa = Some("plugin.alpha");

    for (op, ik, sub) in [
        ("op-plugin-a", "ik-plugin-a", None),
        ("op-plugin-clean", "ik-plugin-clean", None),
        ("op-sub-a", "ik-sub-a", Some(sub_a)),
        ("op-sub-clean", "ik-sub-clean", Some(sub_a)),
        ("op-unrelated", "ik-unrelated", None),
    ] {
        store
            .accept_download_operation(&accept_input(op, ik, sub, Some(dl_id)))
            .unwrap();
    }

    for (tag, hash, op, sub, pid) in [
        ("tag-p-same", "aaaa1001", "op-plugin-a", None, pa),
        ("tag-p-cross", "aaaa1002", "op-plugin-a", None, pa),
        ("tag-s-same", "aaaa1003", "op-sub-a", Some(sub_a), None),
        ("tag-s-cross", "aaaa1004", "op-sub-a", Some(sub_a), None),
    ] {
        store
            .record_download_ownership(&own_input(dl_id, tag, Some(hash), op, sub, pid, owned))
            .unwrap();
    }

    for (tag, expected_op) in [
        ("tag-p-same", "op-plugin-a"),
        ("tag-p-cross", "op-plugin-a"),
        ("tag-s-same", "op-sub-a"),
        ("tag-s-cross", "op-sub-a"),
    ] {
        let res = store.record_download_ownership(&own_input(
            dl_id,
            tag,
            None,
            "op-unrelated",
            None,
            None,
            del,
        ));
        assert!(
            matches!(res, Err(StoreError::Plugin(PluginError::Conflict))),
            "expected Conflict on cross-op ownerless delete for {tag}, got {res:?}"
        );
        let row = store.get_download_ownership(dl_id, tag).unwrap().unwrap();
        assert_eq!(
            (row.ownership_state, row.operation_id.as_str()),
            (owned, expected_op)
        );
    }

    for (tag, op, req_sub, req_pid, exp_sub, exp_pid) in [
        ("tag-p-same", "op-plugin-a", None, None, None, pa),
        ("tag-s-same", "op-sub-a", None, None, Some(sub_a), None),
        ("tag-p-cross", "op-plugin-clean", None, pa, None, pa),
        (
            "tag-s-cross",
            "op-sub-clean",
            Some(sub_a),
            None,
            Some(sub_a),
            None,
        ),
    ] {
        let row = store
            .record_download_ownership(&own_input(dl_id, tag, None, op, req_sub, req_pid, del))
            .unwrap();
        assert_eq!(row.ownership_state, del);
        assert_eq!(row.operation_id, op);
        assert_eq!(
            (row.subscribe_id, row.plugin_id.as_deref()),
            (exp_sub, exp_pid)
        );
    }
}
