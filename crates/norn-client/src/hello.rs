//! The hello, from the dialer's side: the first exchange of every connection
//! this crate opens to a host, verified before a bearer is sent.
//!
//! **A dormant carrier for the 6.3 CLI and the stdio-MCP shim.** Both dial the
//! channel's endpoint, and neither is built: `norn-client` carries only the
//! `serve` grammar until the surfaces land, so nothing in the shipped binary
//! calls [`dial`] yet. It is here now because host verification is a contract
//! both halves must hold from the first host that answers a hello, and the
//! `norn` package's process suite runs one relay matrix against this dialer
//! and the serving crate's takeover dial.
//!
//! **What the two dialers share is `norn-wire`'s**: the request, the reply's
//! reading, the body cap, the loopback test, the comparison of the answer's
//! facts with the connection, and the failure a dial reports
//! (`HelloFailure`). Only the transport and the HMAC check are this crate's.
//!
//! **No bearer without its own verified hello.** [`dial`] opens one
//! connection, sends the hello as its first exchange, and returns a
//! [`Verified`] connection only when the MAC verifies under the chosen token
//! and the facts name this connection; [`Verified::send`] is then the one way
//! to send that token as a bearer, and only on that connection. Every failure
//! closes the connection with the bearer unsent. There is no timeout here:
//! the caller bounds the dial.

use std::fmt;
use std::io;
use std::net::SocketAddr;

use hmac::{Hmac, KeyInit, Mac};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::client::conn::http1::{self, SendRequest};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use norn_config::machine::tokens::Tokens;
use norn_wire::{
    HELLO_BODY_BYTES, HelloAnswer, HelloChannel, HelloExpectation, HelloFailure, HelloMismatch,
    HelloReply, HelloRequest, Nonce, is_dialable,
};
use sha2::Sha256;
use tokio::net::TcpSocket;
use tokio::task::JoinHandle;

/// A connection whose hello verified: the only way this dialer sends a
/// bearer, and only on the connection the hello verified.
pub struct Verified {
    sender: SendRequest<Full<Bytes>>,
    _connection: Connection,
    bearer: HeaderValue,
    host: HeaderValue,
    answer: HelloAnswer,
}

impl fmt::Debug for Verified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Verified")
            .field("answer", &self.answer)
            .finish_non_exhaustive()
    }
}

impl Verified {
    /// The verified answer: the host's channel, build, launch kind and state.
    pub fn answer(&self) -> &HelloAnswer {
        &self.answer
    }

    /// Send `request` on this connection with the bearer, and read its
    /// answer whole. Any `Authorization` the request carried is replaced.
    /// A connection that has closed fails; it is never reopened, because a
    /// new connection needs its own hello.
    pub async fn send(
        &mut self,
        mut request: Request<Full<Bytes>>,
    ) -> Result<Response<Bytes>, hyper::Error> {
        let headers = request.headers_mut();
        headers.insert(AUTHORIZATION, self.bearer.clone());
        headers.entry(HOST).or_insert_with(|| self.host.clone());
        self.sender.ready().await?;
        let response = self.sender.send_request(request).await?;
        let (parts, body) = response.into_parts();
        let body = body.collect().await?.to_bytes();
        Ok(Response::from_parts(parts, body))
    }
}

/// Dial `address`, run the hello with the token [`Tokens::dialer_token`]
/// chooses, and verify the answer against `channel` and this connection.
pub async fn dial(
    address: SocketAddr,
    channel: HelloChannel,
    tokens: &Tokens,
) -> Result<Verified, HelloFailure> {
    if !is_dialable(address) {
        return Err(HelloFailure::NotLoopback(address));
    }
    let token = tokens.dialer_token().ok_or(HelloFailure::NoToken)?;
    let connect = |error| HelloFailure::Connect { address, error };
    let stream = dialer_socket(address)
        .map_err(connect)?
        .connect(address)
        .await
        .map_err(connect)?;
    let local = stream.local_addr().map_err(connect)?;
    let exchange = |error: hyper::Error| HelloFailure::Exchange {
        address,
        error: error.to_string(),
    };
    let (mut sender, connection) = http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
        .await
        .map_err(exchange)?;
    let connection = Connection(tokio::spawn(async move {
        let _ = connection.await;
    }));

    let nonce = fresh_nonce().map_err(|error| HelloFailure::Exchange {
        address,
        error: format!("drawing the nonce failed: {error}"),
    })?;
    let host = HeaderValue::from_str(&address.to_string()).expect("a socket address is a header");
    let body = serde_json::to_vec(&HelloRequest::new(token.label().as_str(), &nonce))
        .expect("a hello request serializes");
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/hello")
        .header(HOST, host.clone())
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("the hello request is well formed");
    let response = sender.send_request(request).await.map_err(exchange)?;
    let status = response.status().as_u16();
    let verification = |mismatch| HelloFailure::Verification { address, mismatch };
    // An answer over the cap is no hello answer; it is not read past it.
    let body = Limited::new(response.into_body(), HELLO_BODY_BYTES)
        .collect()
        .await
        .map_err(|_| verification(HelloMismatch::NotAnAnswer))?
        .to_bytes();
    let answer = match HelloReply::read(status, &body) {
        HelloReply::Answer(answer) => answer,
        HelloReply::Refused => return Err(HelloFailure::HelloRefused { address }),
        HelloReply::TokenFileUnreadable => {
            return Err(HelloFailure::ListenerSaysTokenFileUnreadable { address });
        }
        HelloReply::Unrecognized => return Err(verification(HelloMismatch::NotAnAnswer)),
    };
    // The MAC first: until it verifies, the facts are whoever answered's.
    let mac = answer.mac().ok_or(verification(HelloMismatch::Mac))?;
    if !mac_verifies(token.secret(), &answer.facts().mac_input(&nonce), &mac) {
        return Err(verification(HelloMismatch::Mac));
    }
    answer
        .facts()
        .check_connection(&HelloExpectation {
            channel,
            dialed: address,
            local,
        })
        .map_err(verification)?;

    let mut bearer = HeaderValue::from_str(&format!(
        "Bearer {}",
        norn_wire::hex::encode(token.secret())
    ))
    .expect("lowercase hex is a header value");
    bearer.set_sensitive(true);
    Ok(Verified {
        sender,
        _connection: connection,
        bearer,
        host,
        answer,
    })
}

/// The task driving one connection, aborted when its owner goes: a dial that
/// fails closes its connection at once, bearer unsent.
struct Connection(JoinHandle<()>);

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The options a dialer's socket carries. Set once, on a socket nothing else
/// holds a descriptor of, before it connects.
trait SocketOptions {
    fn set_reuseaddr(&self, reuseaddr: bool) -> io::Result<()>;
}

impl SocketOptions for TcpSocket {
    fn set_reuseaddr(&self, reuseaddr: bool) -> io::Result<()> {
        TcpSocket::set_reuseaddr(self, reuseaddr)
    }
}

/// `SO_REUSEADDR`, so a dialer's socket left in TIME_WAIT on the endpoint
/// cannot make a later start's probe or bind fail.
fn set_options(socket: &impl SocketOptions) -> io::Result<()> {
    socket.set_reuseaddr(true)
}

fn dialer_socket(address: SocketAddr) -> io::Result<TcpSocket> {
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    set_options(&socket)?;
    Ok(socket)
}

/// Whether `tag` is HMAC-SHA256 of `input` under `key`, compared in constant
/// time.
fn mac_verifies(key: &[u8], input: &[u8], tag: &[u8; 32]) -> bool {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(input);
    mac.verify_slice(tag).is_ok()
}

fn fresh_nonce() -> Result<Nonce, getrandom::Error> {
    let mut bytes = [0u8; Nonce::BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(Nonce::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct Recording(Cell<Vec<bool>>);

    impl SocketOptions for Recording {
        fn set_reuseaddr(&self, reuseaddr: bool) -> io::Result<()> {
            let mut calls = self.0.take();
            calls.push(reuseaddr);
            self.0.set(calls);
            Ok(())
        }
    }

    #[test]
    fn the_dialer_sets_reuseaddr_once_before_connecting() {
        let recording = Recording(Cell::new(Vec::new()));
        set_options(&recording).unwrap();
        assert_eq!(recording.0.take(), [true]);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        runtime.block_on(async {
            let socket = dialer_socket("127.0.0.1:1".parse().unwrap()).unwrap();
            assert!(socket.reuseaddr().unwrap());
        });
    }
}
