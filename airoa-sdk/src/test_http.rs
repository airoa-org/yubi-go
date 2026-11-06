//! A backend for the tests that have to see the request.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub(crate) struct Server {
    pub url: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    /// What every request is answered with. 200 unless a test says otherwise;
    /// set it mid-test to make the same backend transiently or permanently
    /// unhappy, which is what the offline queue's contract turns on.
    pub status: Arc<Mutex<u16>>,
    server: Arc<tiny_http::Server>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn new() -> Self {
        // Upstream will not start a take against a real backend with no
        // active_operator.
        Self::answering(
            r#"{"id":"robot","execution_id":"x1","active_operator":{"user_id":"u1","display_name":"op"}}"#,
        )
    }

    /// Every request is answered with `reply`.
    pub fn answering(reply: &'static str) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let url = format!("http://{}", server.server_addr());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let status = Arc::new(Mutex::new(200u16));
        let log = requests.clone();
        let answer = status.clone();
        let worker = server.clone();
        let thread = std::thread::spawn(move || {
            while let Ok(mut request) = worker.recv() {
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                log.lock().unwrap().push(format!("{} {} {body}", request.method(), request.url()));
                let code = *answer.lock().unwrap();
                request
                    .respond(tiny_http::Response::from_string(reply).with_status_code(code))
                    .unwrap();
            }
        });
        Self { url, requests, status, server, thread: Some(thread) }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.server.unblock();
        self.thread.take().unwrap().join().unwrap();
    }
}
