//! Loopback server for ordered and routed scripts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::request::{read_request, RecordedRequest};
use crate::script::{Body, Response, Step};

/// Running loopback gateway. Dropping it aborts the accept loop and every
/// connection task owned by that loop, including permanently stalled tasks.
pub struct MockGateway {
    base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    accepted: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockGateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockGateway {
    /// Serve one connection at a time in script order. Once exhausted, the
    /// final step repeats for subsequent requests.
    pub async fn start_ordered(steps: Vec<Step>) -> Self {
        assert!(!steps.is_empty(), "ordered gateway requires a step");
        let (listener, base_url) = bind_loopback();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicUsize::new(0));

        let task_requests = Arc::clone(&requests);
        let task_accepted = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("Tokio listener");
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let seq = task_accepted.fetch_add(1, Ordering::SeqCst);
                let step = steps
                    .get(seq)
                    .cloned()
                    .unwrap_or_else(|| steps.last().expect("non-empty script").clone());
                serve_one(stream, step, seq, &task_requests).await;
            }
        });
        Self {
            base_url,
            requests,
            accepted,
            task,
        }
    }

    /// Route and serve connections concurrently according to their request.
    pub async fn start_routed<F>(route: F) -> Self
    where
        F: Fn(&RecordedRequest) -> Step + Send + Sync + 'static,
    {
        let (listener, base_url) = bind_loopback();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicUsize::new(0));
        let route = Arc::new(route);

        let task_requests = Arc::clone(&requests);
        let task_accepted = Arc::clone(&accepted);
        let task = tokio::spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("Tokio listener");
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            break;
                        };
                        let seq = task_accepted.fetch_add(1, Ordering::SeqCst);
                        let connection_requests = Arc::clone(&task_requests);
                        let connection_route = Arc::clone(&route);
                        connections.spawn(async move {
                            let Some(mut request) = read_request(&mut stream).await else {
                                return;
                            };
                            request.seq = seq;
                            let step = connection_route(&request);
                            connection_requests
                                .lock()
                                .expect("request recording lock")
                                .push(request);
                            perform_step(stream, step).await;
                        });
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        if completed.is_none() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            base_url,
            requests,
            accepted,
            task,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Complete, parseable requests recorded so far.
    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("request recording lock").len()
    }

    /// Connections accepted so far, including malformed or partial requests.
    pub fn accepted_count(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Complete, parseable requests in stable connection-acceptance order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        let mut requests = self
            .requests
            .lock()
            .expect("request recording lock")
            .clone();
        requests.sort_by_key(|request| request.seq);
        requests
    }
}

fn bind_loopback() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback gateway");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let port = listener.local_addr().expect("listener address").port();
    (listener, format!("http://127.0.0.1:{port}"))
}

async fn serve_one(
    mut stream: TcpStream,
    step: Step,
    seq: usize,
    requests: &Arc<Mutex<Vec<RecordedRequest>>>,
) {
    let Some(mut request) = read_request(&mut stream).await else {
        return;
    };
    request.seq = seq;
    requests
        .lock()
        .expect("request recording lock")
        .push(request);
    perform_step(stream, step).await;
}

async fn perform_step(stream: TcpStream, step: Step) {
    match step {
        Step::Respond(response) => write_response(stream, response).await,
        Step::CloseBeforeHeaders { reset } => hard_close(stream, reset).await,
        Step::Stall => std::future::pending::<()>().await,
    }
}

async fn write_response(mut stream: TcpStream, response: Response) {
    if !response.header_delay.is_zero() {
        tokio::time::sleep(response.header_delay).await;
    }

    let has_content_length = has_header(&response.headers, "content-length");
    let has_connection = has_header(&response.headers, "connection");
    let has_transfer_encoding = has_header(&response.headers, "transfer-encoding");

    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason_phrase(response.status)
    );
    for (name, value) in &response.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    match &response.body {
        Body::Empty if !has_content_length => head.push_str("content-length: 0\r\n"),
        Body::Fixed { data, .. } if !has_content_length => {
            head.push_str(&format!("content-length: {}\r\n", data.len()));
        }
        Body::FixedFragments(frames) if !has_content_length => {
            let length = frames.iter().map(|frame| frame.data.len()).sum::<usize>();
            head.push_str(&format!("content-length: {length}\r\n"));
        }
        Body::FixedThenDrop { declared_len, .. } if !has_content_length => {
            head.push_str(&format!("content-length: {declared_len}\r\n"));
        }
        Body::Chunked(_) | Body::ChunkedThenDrop { .. } if !has_transfer_encoding => {
            head.push_str("transfer-encoding: chunked\r\n");
        }
        _ => {}
    }
    if !has_connection {
        head.push_str("connection: close\r\n");
    }
    head.push_str("\r\n");

    if stream.write_all(head.as_bytes()).await.is_err() || stream.flush().await.is_err() {
        return;
    }

    match response.body {
        Body::Empty => {}
        Body::Fixed { data, delay } => {
            sleep_if_needed(delay).await;
            let _ = stream.write_all(&data).await;
        }
        Body::FixedFragments(frames) | Body::Stream(frames) => {
            for frame in frames {
                sleep_if_needed(frame.delay).await;
                if stream.write_all(&frame.data).await.is_err() {
                    return;
                }
                let _ = stream.flush().await;
                tokio::task::yield_now().await;
            }
        }
        Body::FixedThenDrop { sent, reset, .. } => {
            let _ = stream.write_all(&sent).await;
            let _ = stream.flush().await;
            hard_close(stream, reset).await;
        }
        Body::Chunked(frames) => {
            for frame in frames {
                sleep_if_needed(frame.delay).await;
                if write_chunk(&mut stream, &frame.data).await.is_err() {
                    return;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n").await;
            let _ = stream.flush().await;
        }
        Body::ChunkedThenDrop { frames, reset } => {
            for frame in frames {
                sleep_if_needed(frame.delay).await;
                if write_chunk(&mut stream, &frame.data).await.is_err() {
                    return;
                }
            }
            hard_close(stream, reset).await;
        }
        Body::NeverEnds => std::future::pending::<()>().await,
    }
}

fn has_header(headers: &[(String, String)], expected: &str) -> bool {
    headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(expected))
}

async fn sleep_if_needed(delay: Duration) {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

async fn write_chunk(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    stream
        .write_all(format!("{:x}\r\n", data.len()).as_bytes())
        .await?;
    stream.write_all(data).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await
}

async fn hard_close(mut stream: TcpStream, reset: bool) {
    if reset {
        #[allow(deprecated)]
        let _ = stream.set_linger(Some(Duration::ZERO));
        // Dropping with zero linger emits RST. Calling AsyncWriteExt::shutdown
        // first would request a graceful FIN and defeat this fault mode.
        drop(stream);
        return;
    }
    let _ = stream.shutdown().await;
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}
