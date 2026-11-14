//! The gate-reuse lookup's HTTP listener (`docs/gate-reuse.md`): a thin
//! layer over [`conary_github::reuse`], which answers every request.

use std::io;
use std::net::TcpListener;

use conary_github::reuse::{MAX_BODY, Reply};

use crate::http;

/// The most requests answered at once: each reads GitHub for seconds.
const MOST: usize = 16;

/// Answers every request to `listener` with `answer`, given its
/// `Authorization` header and its body. A request that doesn't read as
/// HTTP gets its status alone.
pub fn serve<F>(listener: TcpListener, answer: F) -> io::Result<()>
where
    F: Fn(Option<&str>, &[u8]) -> Reply + Send + Sync + 'static,
{
    http::serve(listener, MOST, move |mut stream| {
        let reply = match http::read(&mut stream, MAX_BODY, |_| Ok(())) {
            Ok(Ok(request)) => {
                let auth = request.headers.get("authorization").map(String::as_str);
                answer(auth, &request.body)
            }
            Ok(Err(status)) => {
                let _ = http::respond(&mut stream, status, None);
                return;
            }
            Err(_) => {
                let _ = http::respond(&mut stream, 400, None);
                return;
            }
        };
        let body = serde_json::to_vec(&reply.body).unwrap_or_default();
        let _ = http::respond(&mut stream, reply.status, Some(&body));
    })
}
