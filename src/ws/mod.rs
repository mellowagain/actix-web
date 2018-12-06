//! WebSocket protocol support.
//!
//! To setup a `WebSocket`, first do web socket handshake then on success
//! convert `Payload` into a `WsStream` stream and then use `WsWriter` to
//! communicate with the peer.
use std::io;

use failure::Fail;
use http::{header, Method, StatusCode};

use crate::error::ResponseError;
use crate::request::Request;
use crate::response::{Response, ResponseBuilder};

mod client;
mod codec;
mod frame;
mod mask;
mod proto;
mod service;
mod transport;

pub use self::client::{Client, ClientError, Connect, DefaultClient};
pub use self::codec::{Codec, Frame, Message};
pub use self::frame::Parser;
pub use self::proto::{CloseCode, CloseReason, OpCode};
pub use self::service::VerifyWebSockets;
pub use self::transport::Transport;

/// Websocket protocol errors
#[derive(Fail, Debug)]
pub enum ProtocolError {
    /// Received an unmasked frame from client
    #[fail(display = "Received an unmasked frame from client")]
    UnmaskedFrame,
    /// Received a masked frame from server
    #[fail(display = "Received a masked frame from server")]
    MaskedFrame,
    /// Encountered invalid opcode
    #[fail(display = "Invalid opcode: {}", _0)]
    InvalidOpcode(u8),
    /// Invalid control frame length
    #[fail(display = "Invalid control frame length: {}", _0)]
    InvalidLength(usize),
    /// Bad web socket op code
    #[fail(display = "Bad web socket op code")]
    BadOpCode,
    /// A payload reached size limit.
    #[fail(display = "A payload reached size limit.")]
    Overflow,
    /// Continuation is not supported
    #[fail(display = "Continuation is not supported.")]
    NoContinuation,
    /// Bad utf-8 encoding
    #[fail(display = "Bad utf-8 encoding.")]
    BadEncoding,
    /// Io error
    #[fail(display = "io error: {}", _0)]
    Io(#[cause] io::Error),
}

impl ResponseError for ProtocolError {}

impl From<io::Error> for ProtocolError {
    fn from(err: io::Error) -> ProtocolError {
        ProtocolError::Io(err)
    }
}

/// Websocket handshake errors
#[derive(Fail, PartialEq, Debug)]
pub enum HandshakeError {
    /// Only get method is allowed
    #[fail(display = "Method not allowed")]
    GetMethodRequired,
    /// Upgrade header if not set to websocket
    #[fail(display = "Websocket upgrade is expected")]
    NoWebsocketUpgrade,
    /// Connection header is not set to upgrade
    #[fail(display = "Connection upgrade is expected")]
    NoConnectionUpgrade,
    /// Websocket version header is not set
    #[fail(display = "Websocket version header is required")]
    NoVersionHeader,
    /// Unsupported websocket version
    #[fail(display = "Unsupported version")]
    UnsupportedVersion,
    /// Websocket key is not set or wrong
    #[fail(display = "Unknown websocket key")]
    BadWebsocketKey,
}

impl ResponseError for HandshakeError {
    fn error_response(&self) -> Response {
        match *self {
            HandshakeError::GetMethodRequired => Response::MethodNotAllowed()
                .header(header::ALLOW, "GET")
                .finish(),
            HandshakeError::NoWebsocketUpgrade => Response::BadRequest()
                .reason("No WebSocket UPGRADE header found")
                .finish(),
            HandshakeError::NoConnectionUpgrade => Response::BadRequest()
                .reason("No CONNECTION upgrade")
                .finish(),
            HandshakeError::NoVersionHeader => Response::BadRequest()
                .reason("Websocket version header is required")
                .finish(),
            HandshakeError::UnsupportedVersion => Response::BadRequest()
                .reason("Unsupported version")
                .finish(),
            HandshakeError::BadWebsocketKey => {
                Response::BadRequest().reason("Handshake error").finish()
            }
        }
    }
}

/// Verify `WebSocket` handshake request and create handshake reponse.
// /// `protocols` is a sequence of known protocols. On successful handshake,
// /// the returned response headers contain the first protocol in this list
// /// which the server also knows.
pub fn handshake(req: &Request) -> Result<ResponseBuilder, HandshakeError> {
    verify_handshake(req)?;
    Ok(handshake_response(req))
}

/// Verify `WebSocket` handshake request.
// /// `protocols` is a sequence of known protocols. On successful handshake,
// /// the returned response headers contain the first protocol in this list
// /// which the server also knows.
pub fn verify_handshake(req: &Request) -> Result<(), HandshakeError> {
    // WebSocket accepts only GET
    if *req.method() != Method::GET {
        return Err(HandshakeError::GetMethodRequired);
    }

    // Check for "UPGRADE" to websocket header
    let has_hdr = if let Some(hdr) = req.headers().get(header::UPGRADE) {
        if let Ok(s) = hdr.to_str() {
            s.to_lowercase().contains("websocket")
        } else {
            false
        }
    } else {
        false
    };
    if !has_hdr {
        return Err(HandshakeError::NoWebsocketUpgrade);
    }

    // Upgrade connection
    if !req.upgrade() {
        return Err(HandshakeError::NoConnectionUpgrade);
    }

    // check supported version
    if !req.headers().contains_key(header::SEC_WEBSOCKET_VERSION) {
        return Err(HandshakeError::NoVersionHeader);
    }
    let supported_ver = {
        if let Some(hdr) = req.headers().get(header::SEC_WEBSOCKET_VERSION) {
            hdr == "13" || hdr == "8" || hdr == "7"
        } else {
            false
        }
    };
    if !supported_ver {
        return Err(HandshakeError::UnsupportedVersion);
    }

    // check client handshake for validity
    if !req.headers().contains_key(header::SEC_WEBSOCKET_KEY) {
        return Err(HandshakeError::BadWebsocketKey);
    }
    Ok(())
}

/// Create websocket's handshake response
///
/// This function returns handshake `Response`, ready to send to peer.
pub fn handshake_response(req: &Request) -> ResponseBuilder {
    let key = {
        let key = req.headers().get(header::SEC_WEBSOCKET_KEY).unwrap();
        proto::hash_key(key.as_ref())
    };

    Response::build(StatusCode::SWITCHING_PROTOCOLS)
        .upgrade("websocket")
        .header(header::TRANSFER_ENCODING, "chunked")
        .header(header::SEC_WEBSOCKET_ACCEPT, key.as_str())
        .take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{header, Method};
    use test::TestRequest;

    #[test]
    fn test_handshake() {
        let req = TestRequest::default().method(Method::POST).finish();
        assert_eq!(
            HandshakeError::GetMethodRequired,
            verify_handshake(&req).err().unwrap()
        );

        let req = TestRequest::default().finish();
        assert_eq!(
            HandshakeError::NoWebsocketUpgrade,
            verify_handshake(&req).err().unwrap()
        );

        let req = TestRequest::default()
            .header(header::UPGRADE, header::HeaderValue::from_static("test"))
            .finish();
        assert_eq!(
            HandshakeError::NoWebsocketUpgrade,
            verify_handshake(&req).err().unwrap()
        );

        let req = TestRequest::default()
            .header(
                header::UPGRADE,
                header::HeaderValue::from_static("websocket"),
            )
            .finish();
        assert_eq!(
            HandshakeError::NoConnectionUpgrade,
            verify_handshake(&req).err().unwrap()
        );

        let req = TestRequest::default()
            .header(
                header::UPGRADE,
                header::HeaderValue::from_static("websocket"),
            )
            .header(
                header::CONNECTION,
                header::HeaderValue::from_static("upgrade"),
            )
            .finish();
        assert_eq!(
            HandshakeError::NoVersionHeader,
            verify_handshake(&req).err().unwrap()
        );

        let req = TestRequest::default()
            .header(
                header::UPGRADE,
                header::HeaderValue::from_static("websocket"),
            )
            .header(
                header::CONNECTION,
                header::HeaderValue::from_static("upgrade"),
            )
            .header(
                header::SEC_WEBSOCKET_VERSION,
                header::HeaderValue::from_static("5"),
            )
            .finish();
        assert_eq!(
            HandshakeError::UnsupportedVersion,
            verify_handshake(&req).err().unwrap()
        );

        let req = TestRequest::default()
            .header(
                header::UPGRADE,
                header::HeaderValue::from_static("websocket"),
            )
            .header(
                header::CONNECTION,
                header::HeaderValue::from_static("upgrade"),
            )
            .header(
                header::SEC_WEBSOCKET_VERSION,
                header::HeaderValue::from_static("13"),
            )
            .finish();
        assert_eq!(
            HandshakeError::BadWebsocketKey,
            verify_handshake(&req).err().unwrap()
        );

        let req = TestRequest::default()
            .header(
                header::UPGRADE,
                header::HeaderValue::from_static("websocket"),
            )
            .header(
                header::CONNECTION,
                header::HeaderValue::from_static("upgrade"),
            )
            .header(
                header::SEC_WEBSOCKET_VERSION,
                header::HeaderValue::from_static("13"),
            )
            .header(
                header::SEC_WEBSOCKET_KEY,
                header::HeaderValue::from_static("13"),
            )
            .finish();
        assert_eq!(
            StatusCode::SWITCHING_PROTOCOLS,
            handshake_response(&req).finish().status()
        );
    }

    #[test]
    fn test_wserror_http_response() {
        let resp: Response = HandshakeError::GetMethodRequired.error_response();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        let resp: Response = HandshakeError::NoWebsocketUpgrade.error_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp: Response = HandshakeError::NoConnectionUpgrade.error_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp: Response = HandshakeError::NoVersionHeader.error_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp: Response = HandshakeError::UnsupportedVersion.error_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp: Response = HandshakeError::BadWebsocketKey.error_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
