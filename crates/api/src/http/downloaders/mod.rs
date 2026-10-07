//! Downloaders: qBittorrent / Transmission instances, live tasks, submit.

mod instances;
mod submission;
mod task_listing;
mod tasks;

pub(crate) use instances::{
    create_downloader, delete_downloader, get_downloader, get_limits, list_downloaders,
    patch_downloader, put_target_pref, set_limits, target_prefs, verify_downloader,
};
pub(crate) use submission::submit;
pub(crate) use task_listing::list_tasks;
pub(crate) use tasks::{delete_task, pause_task, remove_task, replace_task, resume_task};
