use std::sync::Arc;

use parking_lot::Mutex;

pub(crate) fn transmission_fixture() -> (
    String,
    Arc<Mutex<Vec<String>>>,
    crate::lifecycle::FakeServer,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let server = crate::lifecycle::FakeServer::start(move |request, stream| {
        recorded.lock().push(request.raw.clone());
        let body = if request.raw.contains("torrent-get") {
            r#"{"result":"success","arguments":{"torrents":[{"id":1,"name":"The.Matrix.1999.1080p","hashString":"0123456789abcdef0123456789abcdef01234567","sizeWhenDone":100,"labels":["crawler-media"]}]}}"#
        } else {
            r#"{"result":"success","arguments":{"rpc-version":16}}"#
        };
        crate::lifecycle::respond(
            stream,
            "200 OK",
            "X-Transmission-Session-Id: test\r\n",
            body.as_bytes(),
        );
    });
    (format!("{}/transmission/rpc", server.url()), calls, server)
}
