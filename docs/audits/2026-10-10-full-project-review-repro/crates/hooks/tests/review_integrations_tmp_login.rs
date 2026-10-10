use std::sync::Mutex;
use domain::{Site, SiteId};
use hooks::{Bus, HttpPost, LoginPlugin, CheckInPlugin, PluginError, SiteCredentials};

struct FakeSite(Mutex<Site>);
impl SiteCredentials for FakeSite {
    fn load(&self, _: SiteId) -> Result<Site, PluginError> { Ok(self.0.lock().unwrap().clone()) }
    fn save(&self, site: &Site) -> Result<(), PluginError> {
        *self.0.lock().unwrap() = site.clone();
        Ok(())
    }
}
struct LoginPage;
impl HttpPost for LoginPage {
    fn post(&self, _: &str, _: &str, _: Option<&str>, _: Option<&str>) -> Result<String, PluginError> {
        Ok("<html><form action='takelogin.php'><input name='password'/></form></html>".into())
    }
}
fn site() -> FakeSite {
    FakeSite(Mutex::new(Site {
        id: SiteId::new(), name: "fixture".into(), url: "https://pt.invalid".into(),
        profile_id: "demo".into(), cookie: None, api_key: None, rss_url: None,
        proxy: None, rate_limit_per_minute: None, cdp_url: None, downloader_id: None,
        enabled: true,
    }))
}
#[test]
fn login_page_is_not_a_successful_login() {
    let store = site();
    let id = store.0.lock().unwrap().id;
    assert!(LoginPlugin::http(&Bus::new(), &store, &LoginPage).login(id).is_err());
}
#[test]
fn login_page_is_not_a_successful_check_in() {
    let store = site();
    let id = store.0.lock().unwrap().id;
    assert!(CheckInPlugin::http(&Bus::new(), &store, &LoginPage).check_in(id).is_err());
}
