//! The HTTP/1.1 connection handler.
//!
//! No route is served yet: every request is answered 404 Not Found, with an
//! empty body. The hello, the bearer middleware and the wire route take their
//! paths in later tasks of this phase.

use std::convert::Infallible;

use http_body_util::Empty;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

/// Serve one accepted connection until its client closes it.
pub(crate) async fn answer(stream: TcpStream) {
    // A connection that fails mid-exchange is the client's to retry; there
    // is no request whose outcome it could leave unknown.
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service_fn(not_found))
        .await;
}

async fn not_found(
    _request: Request<Incoming>,
) -> Result<Response<Empty<&'static [u8]>>, Infallible> {
    let mut response = Response::new(Empty::new());
    *response.status_mut() = StatusCode::NOT_FOUND;
    Ok(response)
}
