//! NDJSON framing: roundtrip, oversized lines, malformed input, EOF rules.

use a4aa::framing::{self, FrameError};
use serde_json::{json, Value};
use tokio::io::BufReader;

#[tokio::test]
async fn roundtrip_writes_line_and_reads_value() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let mut reader = BufReader::new(reader);
    let frame = json!({"jsonrpc": "2.0", "id": 7, "method": "ping", "params": {"x": "y"}});
    framing::write_frame(&mut writer, &frame).await.unwrap();
    drop(writer);
    let read = framing::read_frame(&mut reader, 1024)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(read, frame);
    // EOF after the frame.
    assert!(framing::read_frame(&mut reader, 1024)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn blank_lines_are_skipped() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let mut reader = BufReader::new(reader);
    writer.write_all(b"\n\n\n").await.unwrap();
    framing::write_frame(&mut writer, &json!({"ok": true}))
        .await
        .unwrap();
    drop(writer);
    let read = framing::read_frame(&mut reader, 1024)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(read, json!({"ok": true}));
}

use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn oversized_line_is_dropped_and_stream_continues() {
    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let mut reader = BufReader::new(reader);
    let big = "x".repeat(5000);
    writer
        .write_all(format!("{{\"pad\":\"{big}\"}}\n").as_bytes())
        .await
        .unwrap();
    framing::write_frame(&mut writer, &json!({"after": 1}))
        .await
        .unwrap();
    drop(writer);
    match framing::read_frame(&mut reader, 1024).await.unwrap() {
        Some(Err(FrameError::Oversized(_))) => {}
        other => panic!("expected oversized frame, got {other:?}"),
    }
    let next = framing::read_frame(&mut reader, 1024)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(next, json!({"after": 1}));
}

#[tokio::test]
async fn unterminated_oversized_line_is_dropped_at_eof() {
    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let mut reader = BufReader::new(reader);
    let big = "y".repeat(5000);
    writer.write_all(big.as_bytes()).await.unwrap(); // no newline
    drop(writer);
    match framing::read_frame(&mut reader, 1024).await.unwrap() {
        Some(Err(_)) => {}
        other => panic!("expected error frame, got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_json_is_reported_not_fatal() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let mut reader = BufReader::new(reader);
    writer.write_all(b"{not json}\n").await.unwrap();
    framing::write_frame(&mut writer, &json!(42)).await.unwrap();
    drop(writer);
    match framing::read_frame(&mut reader, 1024).await.unwrap() {
        Some(Err(FrameError::Invalid(_))) => {}
        other => panic!("expected invalid frame, got {other:?}"),
    }
    assert_eq!(
        framing::read_frame(&mut reader, 1024)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Value::Number(42.into())
    );
}

#[tokio::test]
async fn unterminated_trailing_line_is_delivered_at_eof() {
    let (mut writer, reader) = tokio::io::duplex(4096);
    let mut reader = BufReader::new(reader);
    writer.write_all(b"{\"tail\":1}").await.unwrap(); // no newline
    drop(writer);
    let read = framing::read_frame(&mut reader, 1024)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(read, json!({"tail": 1}));
    assert!(framing::read_frame(&mut reader, 1024)
        .await
        .unwrap()
        .is_none());
}
