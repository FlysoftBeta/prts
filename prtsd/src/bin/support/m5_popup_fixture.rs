impl InputFixture {
    // A genuinely static page for exact popup restoration comparisons. The
    // OOPIF input fixture can report script readiness before its first paint.
    fn static_popup_page() -> Result<Self> {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    workers.push(thread::spawn(move || {
                        let _=stream.set_nonblocking(false);
                        let _=stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let _=stream.set_write_timeout(Some(Duration::from_secs(5)));
                        let mut request=[0;8192];
                        let mut n=0;
                        while n<request.len() && !request[..n].windows(4).any(|v|v==b"\r\n\r\n") {
                            match stream.read(&mut request[n..]) { Ok(0)|Err(_)=>return,Ok(count)=>n+=count }
                        }
                        let blue=String::from_utf8_lossy(&request[..n]).lines().next().is_some_and(|line|line.contains("/blue"));
                        let (color,name)=if blue {("20,60,210","blue")} else {("220,30,60","red")};
                        let body=format!("<!doctype html><meta charset=utf-8><title>popup-{name}</title><body style='margin:0;background:rgb({color})'>");
                        let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",body.len());
                        let _=stream.write_all(response.as_bytes());
                    }));
                }
                thread::sleep(Duration::from_millis(5));
            }
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(Self {
            stop,
            thread: Some(thread),
            port,
        })
    }
}
