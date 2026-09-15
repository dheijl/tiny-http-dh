use criterion::{Criterion, criterion_group, criterion_main};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use tiny_http_dh::Method;

/// Reads a stream to completion and discards the bytes.
fn drain(mut stream: TcpStream) {
    let mut buf = [0u8; 512];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

fn sequential_requests(c: &mut Criterion) {
    let server = tiny_http_dh::Server::http("0.0.0.0:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    // Draining the responses is left to a background thread reading a cloned
    // handle, so it doesn't count towards the measured iteration time and the
    // client's kernel receive buffer never fills up. Left unread, the server's
    // writer eventually blocks on a full socket buffer while this thread waits
    // to send the next request, deadlocking the benchmark.
    let read_stream = stream.try_clone().unwrap();
    thread::spawn(move || drain(read_stream));

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
    let _ = fdlimit::raise_fd_limit();

    let server = tiny_http_dh::Server::http("0.0.0.0:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();

    // Each iteration's 1000 connections are handed off here to be drained on a
    // background thread, so reading the (already-buffered, tiny) responses
    // doesn't count towards the measured iteration time and the streams
    // aren't dropped - and RST'd - with unread data still in their buffers.
    let (drain_tx, drain_rx) = mpsc::channel::<TcpStream>();
    thread::spawn(move || {
        for stream in drain_rx {
            drain(stream);
        }
    });

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

            for stream in streams {
                let _ = drain_tx.send(stream);
            }
        });
    });
}

criterion_group!(benches, sequential_requests, parallel_requests);
criterion_main!(benches);
