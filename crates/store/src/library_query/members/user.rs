use crate::library_query::items::Bindings;
use domain::UserId;

pub(crate) fn user_join(bindings: &mut Bindings, user: Option<UserId>, needed: bool) -> String {
    if !needed {
        return String::new();
    }
    let user = bindings.push(user.map(|id| id.to_string()).unwrap_or_default().into());
    format!(
        " LEFT JOIN user_library_items u ON u.library_id=i.library_id AND u.media_id=i.media_id AND u.user_id={user}"
    )
}
