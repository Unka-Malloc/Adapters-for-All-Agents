//! Minimal RFC 6455 WebSocket client for agent-owned loopback servers
//! (for example `droid daemon --port <n>` speaking JSON-RPC over text
//! frames).
//!
//! Scope is deliberately narrow: `ws://` only (loopback daemons never
//! need TLS), text messages only, one WS message carries exactly one
//! JSON document. The peer-facing side is a pair of in-memory duplex
//! streams — inbound messages surface as newline-terminated documents so
//! `crate::framing` sees ordinary NDJSON; each complete outbound line is
//! sent as one masked text frame. A pump task owns the socket and the
//! frame codec: ping frames are answered with pong, a close frame is
//! echoed and ends the pump (the reader half then sees EOF).
//!
//! Masking uses a rolling counter — the mask exists to defeat cache
//! poisoning in intermediaries, never to authenticate the payload, and a
//! loopback hop has no intermediaries.

use crate::error::AdapterError;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::TcpStream;

/// One established WebSocket connection, exposed as byte streams. The pump
/// exits on its own once either half reaches EOF — dropping or closing the
/// streams (or losing the socket) is all the teardown the pump needs.
pub(crate) struct WsPeer {
    /// Reads decoded message payloads (`\n`-terminated per message).
    pub reader: DuplexStream,
    /// Writes become masked text frames, one complete line per message.
    pub writer: DuplexStream,
}

/// Connect `ws://host:port/path`, run the upgrade handshake, then split
/// the socket into the peer-facing byte streams.
pub(crate) async fn connect(url: &str, max_message: usize) -> Result<WsPeer, AdapterError> {
    let (authority, path) = parse_ws_url(url)?;
    let mut socket = TcpStream::connect(authority)
        .await
        .map_err(|error| AdapterError::io("agent_protocol_failed", "ws/connect", error))?;
    handshake(&mut socket, authority, path).await?;
    let (reader, mut to_peer) = tokio::io::duplex(64 * 1024);
    let (mut from_peer, writer) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let _ = pump(&mut socket, &mut to_peer, &mut from_peer, max_message).await;
    });
    Ok(WsPeer { reader, writer })
}

/// `ws://host:port/path` → ((host:port), path). Only the loopback form
/// an agent daemon announces is supported; `wss://` and exotic URLs fail
/// closed rather than degrading silently.
fn parse_ws_url(url: &str) -> Result<(&str, &str), AdapterError> {
    let rest = url.strip_prefix("ws://").ok_or_else(|| {
        AdapterError::permanent("agent_protocol_failed", "ws/connect")
            .with_detail(format!("unsupported websocket url: {url}"))
    })?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || !authority.contains(':') {
        return Err(
            AdapterError::permanent("agent_protocol_failed", "ws/connect")
                .with_detail(format!("websocket url lacks host:port: {url}")),
        );
    }
    if !crate::transport::is_loopback_authority(authority) {
        return Err(
            AdapterError::permanent("agent_protocol_failed", "ws/connect")
                .with_detail(format!("websocket endpoint is not loopback: {url}")),
        );
    }
    Ok((authority, path))
}

/// RFC 6455 §4.1 client handshake: send the upgrade request, expect
/// `101`. The `Sec-WebSocket-Accept` digest is advisory for a loopback
/// hop — a non-101 status already fails closed.
async fn handshake(
    socket: &mut TcpStream,
    authority: &str,
    path: &str,
) -> Result<(), AdapterError> {
    // 16 bytes of key material: a counter+xor of cheap entropy sources —
    // validity requires only that it be 16 base64'd bytes.
    let mut key = [0u8; 16];
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64) << 32;
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = (seed >> (i % 8 * 8)) as u8 ^ (i as u8).wrapping_mul(97);
    }
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        base64(&key)
    );
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|error| AdapterError::io("agent_protocol_failed", "ws/handshake", error))?;
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 4096 {
            return Err(
                AdapterError::permanent("agent_protocol_failed", "ws/handshake")
                    .with_detail("websocket upgrade response never terminated"),
            );
        }
        match socket.read(&mut byte).await {
            Ok(0) => {
                return Err(
                    AdapterError::permanent("agent_protocol_failed", "ws/handshake")
                        .with_detail("websocket upgrade closed before a response"),
                )
            }
            Ok(_) => head.push(byte[0]),
            Err(error) => {
                return Err(AdapterError::io(
                    "agent_protocol_failed",
                    "ws/handshake",
                    error,
                ))
            }
        }
    }
    let status = String::from_utf8_lossy(&head);
    if !status
        .lines()
        .next()
        .is_some_and(|line| line.contains(" 101"))
    {
        return Err(
            AdapterError::permanent("agent_protocol_failed", "ws/handshake").with_detail(format!(
                "websocket upgrade refused: {}",
                status.lines().next().unwrap_or("<empty>")
            )),
        );
    }
    Ok(())
}

/// The socket pump: decoded inbound messages flow into `to_peer`
/// (newline-terminated), complete outbound lines from `from_peer` become
/// masked text frames. Returns on socket EOF/error or when the peer
/// drops its halves.
async fn pump<S>(
    socket: &mut S,
    to_peer: &mut DuplexStream,
    from_peer: &mut DuplexStream,
    max_message: usize,
) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut inbound = Vec::with_capacity(8 * 1024);
    let mut outbound = Vec::with_capacity(4 * 1024);
    // Fragmented-message reassembly: control frames may interleave.
    let mut fragmented: Option<Vec<u8>> = None;
    let mut mask_serial: u32 = 0xA4AA_0001;
    loop {
        tokio::select! {
            read = socket.read_buf(&mut inbound) => {
                match read {
                    Ok(0) | Err(_) => return Ok(()),
                    Ok(_) => {}
                }
                loop {
                    match take_frame(&mut inbound, max_message)? {
                        None => break,
                        Some(Frame { opcode, fin, payload }) => {
                            match opcode {
                                OP_TEXT | OP_BINARY | OP_CONT => {
                                    let message = if opcode == OP_CONT || fragmented.is_some() {
                                        let acc = fragmented.get_or_insert_with(Vec::new);
                                        if acc.len() + payload.len() > max_message {
                                            return Err(std::io::Error::new(
                                                std::io::ErrorKind::InvalidData,
                                                "websocket message exceeds the lane limit",
                                            ));
                                        }
                                        acc.extend_from_slice(&payload);
                                        if fin { fragmented.take() } else { continue; }
                                    } else if fin {
                                        Some(payload)
                                    } else {
                                        fragmented = Some(payload);
                                        continue;
                                    };
                                    if let Some(mut message) = message {
                                        if !message.ends_with(b"\n") {
                                            message.push(b'\n');
                                        }
                                        if to_peer.write_all(&message).await.is_err() {
                                            return Ok(());
                                        }
                                    }
                                }
                                OP_PING => {
                                    socket
                                        .write_all(&encode_frame(OP_PONG, &payload, &mut mask_serial))
                                        .await?;
                                }
                                OP_CLOSE => {
                                    let _ = socket
                                        .write_all(&encode_frame(OP_CLOSE, &payload, &mut mask_serial))
                                        .await;
                                    return Ok(());
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            read = from_peer.read_buf(&mut outbound) => {
                match read {
                    Ok(0) => return Ok(()),
                    Err(_) => return Ok(()),
                    Ok(_) => {}
                }
                while let Some(end) = outbound.iter().position(|b| *b == b'\n') {
                    let mut line: Vec<u8> = outbound.drain(..=end).collect();
                    line.pop();
                    if line.is_empty() {
                        continue;
                    }
                    socket
                        .write_all(&encode_frame(OP_TEXT, &line, &mut mask_serial))
                        .await?;
                }
                // A partial line still counts: flush it as its own
                // message when the peer's write did not end in a newline
                // — the lane always writes complete NDJSON lines, so this
                // only fires on abrupt close.
            }
        }
    }
}

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

struct Frame {
    opcode: u8,
    fin: bool,
    payload: Vec<u8>,
}

/// Pop one complete frame off `inbound` (leave partial bytes buffered).
/// Server frames are unmasked; masked input is tolerated anyway. A frame
/// declaring more than `max_message` payload bytes is refused up front so
/// a hostile length field cannot grow the buffer without bound.
fn take_frame(inbound: &mut Vec<u8>, max_message: usize) -> std::io::Result<Option<Frame>> {
    if inbound.len() < 2 {
        return Ok(None);
    }
    let fin = inbound[0] & 0x80 != 0;
    let opcode = inbound[0] & 0x0f;
    let masked = inbound[1] & 0x80 != 0;
    let mut len = (inbound[1] & 0x7f) as u64;
    let mut head = 2;
    if len == 126 {
        if inbound.len() < 4 {
            return Ok(None);
        }
        len = u16::from_be_bytes([inbound[2], inbound[3]]) as u64;
        head = 4;
    } else if len == 127 {
        if inbound.len() < 10 {
            return Ok(None);
        }
        len = u64::from_be_bytes(inbound[2..10].try_into().unwrap());
        head = 10;
    }
    let mask_at = head;
    if masked {
        head += 4;
    }
    // A corrupt 64-bit length must not wrap the buffered-byte check.
    let need = (head as u64).checked_add(len).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "websocket frame length overflow",
        )
    })?;
    if len > max_message as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "websocket frame exceeds the lane limit",
        ));
    }
    if (inbound.len() as u64) < need {
        return Ok(None);
    }
    let mask = if masked {
        Some([
            inbound[mask_at],
            inbound[mask_at + 1],
            inbound[mask_at + 2],
            inbound[mask_at + 3],
        ])
    } else {
        None
    };
    let mut payload: Vec<u8> = inbound.drain(..head + len as usize).skip(head).collect();
    if let Some(mask) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok(Some(Frame {
        opcode,
        fin,
        payload,
    }))
}

/// Encode one masked outbound frame (`mask_serial` rotates so successive
/// frames never repeat a key).
fn encode_frame(opcode: u8, payload: &[u8], mask_serial: &mut u32) -> Vec<u8> {
    *mask_serial = mask_serial.wrapping_add(0x9E37_79B9);
    let mask = mask_serial.to_be_bytes();
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode);
    let len = payload.len();
    if len < 126 {
        out.push(0x80 | len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// RFC 4648 base64 — shared with the serve lane's basic-auth helper.
fn base64(data: &[u8]) -> String {
    crate::transport::base64(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ws_url_accepts_loopback_only() {
        assert!(parse_ws_url("ws://127.0.0.1:9229/rpc").is_ok());
        assert!(parse_ws_url("ws://localhost:3000").is_ok());
        assert!(parse_ws_url("ws://[::1]:8080/").is_ok());
        // An announced endpoint off this host would receive the lane's
        // credentials over a cleartext socket — fail closed.
        for hostile in [
            "ws://203.0.113.9:80/",
            "ws://attacker.example:443/",
            "ws://0.0.0.0:9000/",
            "ws://127.0.0.2:9000/",
            "wss://127.0.0.1:443/",
        ] {
            assert!(parse_ws_url(hostile).is_err(), "{hostile} must fail closed");
        }
    }

    /// Decode a server→client (unmasked) frame built by hand.
    #[test]
    fn take_frame_decodes_extended_lengths() {
        let mut serial = 7;
        // 126-length path
        let payload = vec![b'x'; 300];
        let mut wire = vec![0x81, 126];
        wire.extend_from_slice(&(300u16).to_be_bytes());
        wire.extend_from_slice(&payload);
        let frame = take_frame(&mut wire, 1024).unwrap().unwrap();
        assert_eq!(frame.opcode, OP_TEXT);
        assert!(frame.fin);
        assert_eq!(frame.payload, payload);
        assert!(wire.is_empty());
        // masked roundtrip through encode_frame
        let encoded = encode_frame(OP_TEXT, b"hello", &mut serial);
        let mut wire = encoded;
        let frame = take_frame(&mut wire, 1024).unwrap().unwrap();
        assert_eq!(frame.payload, b"hello");
    }

    #[test]
    fn take_frame_waits_for_partial_bytes() {
        let mut wire = vec![0x81];
        assert!(take_frame(&mut wire, 1024).unwrap().is_none());
        wire.extend_from_slice(&[0x80 | 5]); // masked, len 5
        assert!(take_frame(&mut wire, 1024).unwrap().is_none());
    }

    #[test]
    fn take_frame_refuses_oversize_declared_length() {
        let mut wire = vec![0x81, 126];
        wire.extend_from_slice(&2048u16.to_be_bytes());
        wire.extend_from_slice(&[0u8; 8]);
        assert!(take_frame(&mut wire, 1024).is_err());
        assert!(take_frame(&mut wire, 1024 * 1024).unwrap().is_none());
    }

    #[test]
    fn take_frame_rejects_wrapping_length() {
        // 127-length u64::MAX + head would wrap the need check.
        let mut wire = vec![0x81, 127];
        wire.extend_from_slice(&u64::MAX.to_be_bytes());
        wire.extend_from_slice(&[0u8; 4]);
        assert!(take_frame(&mut wire, 1024).is_err());
    }

    #[tokio::test]
    async fn pump_bridges_frames_and_lines() {
        let (mut socket_a, mut socket_b) = tokio::io::duplex(64 * 1024);
        let (reader, mut to_peer) = tokio::io::duplex(64 * 1024);
        let (mut from_peer, writer) = tokio::io::duplex(64 * 1024);
        let pump_task =
            tokio::spawn(
                async move { pump(&mut socket_a, &mut to_peer, &mut from_peer, 1024).await },
            );
        // socket side → peer: a server text frame arrives as one line.
        socket_b.write_all(&[0x81, 0x05]).await.unwrap();
        socket_b.write_all(b"hello").await.unwrap();
        let mut reader = reader;
        let mut buf = [0u8; 8];
        let n = AsyncReadExt::read(&mut reader, &mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello\n");

        // peer → socket side: a complete line leaves as one masked text frame.
        let mut writer = writer;
        writer.write_all(b"{\"id\":1}\n").await.unwrap();
        let frame = {
            let mut head = [0u8; 2];
            socket_b.read_exact(&mut head).await.unwrap();
            assert_eq!(head[0], 0x81);
            let len = head[1] & 0x7f;
            let mut rest = vec![0u8; 4 + len as usize];
            socket_b.read_exact(&mut rest).await.unwrap();
            let mask = &rest[..4];
            rest[4..]
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ mask[i % 4])
                .collect::<Vec<u8>>()
        };
        assert_eq!(frame, br#"{"id":1}"#);
        drop(reader);
        drop(writer);
        pump_task.await.unwrap().unwrap();
    }
}
