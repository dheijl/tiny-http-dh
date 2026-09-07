use criterion::{Criterion, criterion_group, criterion_main};
use std::io::Write;
use tiny_http_dh::Method;

fn sequential_requests(c: &mut Criterion) {
    let server = tiny_http_dh::Server::http("0.0.0.0:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();

    c.bench_function("sequential_requests", |b| {
        b.iter(|| {
            (write!(stream, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")).unwrap();

            let request = server.recv().unwrap();

            assert_eq!(request.method(), &Method::Get);

            let _ = request.respond(tiny_http_dh::Response::new_empty(tiny_http_dh::StatusCode(
                204,
            )));
        });
    });
}

fn parallel_requests(c: &mut Criterion) {
    fdlimit::raise_fd_limit();

    let server = tiny_http_dh::Server::http("0.0.0.0:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();

    c.bench_function("parallel_requests", |b| {
        b.iter(|| {
            let mut streams = Vec::new();

            for _ in 0..1000usize {
                let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
                (write!(
                    stream,
                    "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                ))
                .unwrap();
                streams.push(stream);
            }

            loop {
                let request = match server.try_recv().unwrap() {
                    None => break,
                    Some(rq) => rq,
                };

                assert_eq!(request.method(), &Method::Get);

                let _ = request.respond(tiny_http_dh::Response::new_empty(
                    tiny_http_dh::StatusCode(204),
                ));
            }
        });
    });
}

criterion_group!(benches, sequential_requests, parallel_requests);
criterion_main!(benches);
