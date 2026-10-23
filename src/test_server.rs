//! A tiny HTTP server for tests. It serves files from a temporary folder,
//! redirects `/moved` to `/sub/index.sx`, and counts requests.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

pub struct TestServer {
    base: String,
    dir: PathBuf,
    requests: Arc<AtomicUsize>,
}

impl TestServer {
    /// Serves `files`, given as (path, contents).
    pub fn start(files: &[(&str, &str)]) -> TestServer {
        static SERVERS: AtomicUsize = AtomicUsize::new(0);
        let n = SERVERS.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("toy-browser-server-{}-{n}", std::process::id()));
        for (path, contents) in files {
            let file = dir.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, contents).unwrap();
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let (root, count) = (dir.clone(), requests.clone());
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                count.fetch_add(1, Ordering::Relaxed);
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap_or_default();
                // Skip the headers, up to the blank line.
                let mut header = String::new();
                while reader.read_line(&mut header).unwrap_or_default() > 2 {
                    header.clear();
                }
                let target = request.split_whitespace().nth(1).unwrap_or("/");
                let path = target.split('?').next().unwrap_or("/");
                let (status, extra, body) = match fs::read(root.join(path.trim_start_matches('/'))) {
                    _ if path == "/moved" => ("302 Found", "Location: /sub/index.sx\r\n", Vec::new()),
                    Ok(body) => ("200 OK", "", body),
                    Err(_) => ("404 Not Found", "", b"not found".to_vec()),
                };
                let head = format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&body));
            }
        });
        TestServer { base, dir, requests }
    }

    /// Serves another file, which can be binary.
    pub fn add(&self, path: &str, contents: &[u8]) {
        fs::write(self.dir.join(path), contents).unwrap();
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
