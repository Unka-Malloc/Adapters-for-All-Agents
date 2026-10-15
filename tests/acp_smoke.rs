//! Minimal ACP handshake repro: spawn fixture, connect, initialize.

#[path = "support/settings.rs"]
mod test_settings;

use agent_client_protocol::schema::v1::InitializeRequest;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, ByteStreams, ConnectionTo};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::test]
async fn raw_acp_initialize_handshake() {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_a4aa"))
        .arg("fixture-agent")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn fixture");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    let result = tokio::time::timeout(
        test_settings::WAIT_BUDGET,
        agent_client_protocol::Client
            .builder()
            .name("a4aa-smoke")
            .connect_with(
                ByteStreams::new(stdin.compat_write(), stdout.compat()),
                async move |connection: ConnectionTo<Agent>| {
                    let initialized = connection
                        .send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;
                    Ok(initialized.protocol_version)
                },
            ),
    )
    .await
    .expect("handshake timed out")
    .expect("initialize failed");
    assert_eq!(result, ProtocolVersion::V1);
    let _ = child.kill().await;
}
