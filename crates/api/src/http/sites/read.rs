//! Site read paths: list / detail and the admin-visibility helper.

use super::*;

pub(crate) async fn list_sites(
    State(state): State<ApiState>,
    Extension(user_id): Extension<domain::UserId>,
) -> Response {
    crate::http::store_wait::store_read(&state, move |store| {
        let sites = match store.list_sites() {
            Ok(sites) => sites,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        let admin = site_admin(&store, user_id);
        ok(Value::Array(
            sites
                .iter()
                .map(|site| site_public(site, auth_type(site), admin))
                .collect(),
        ))
        .into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}

fn site_admin(store: &crate::Store, user_id: domain::UserId) -> bool {
    match store.user_role(user_id) {
        Ok(role) => role == "admin",
        Err(error) => {
            tracing::error!(%user_id, %error, "Failed to resolve Site credential access");
            false
        }
    }
}

pub(crate) async fn get_site(
    State(state): State<ApiState>,
    Extension(user_id): Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    crate::http::store_wait::store_read(&state, move |store| {
        let Some(site) = store.get_site(id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
        };
        ok(site_public(
            &site,
            auth_type(&site),
            site_admin(&store, user_id),
        ))
        .into_response()
    })
    .await
    .unwrap_or_else(|error| error.response())
}
