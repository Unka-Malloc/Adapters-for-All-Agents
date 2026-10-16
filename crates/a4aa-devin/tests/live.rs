//! Live wiring check against a real `devin acp` process — ignored by
//! default, run explicitly where Devin is installed and a credential is
//! available:
//!
//! ```sh
//! DEVIN_API_KEY=… cargo test -p a4aa-devin --test live -- --ignored
//! ```
//!
//! `DEVIN_BINARY` overrides `command.program` when `devin` is not on PATH
//! (for example the Devin.app bundled binary). Nothing is written outside
//! the agent's own session state; the session closes before the test ends.

use a4aa::{Adapter, AgentEvent, SessionRequest, TurnRequest};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[tokio::test]
#[ignore = "live check: needs a Devin install and DEVIN_API_KEY"]
async fn devin_acp_session_round_trip() {
    let mut spec = a4aa_devin::spec();
    if let Ok(binary) = std::env::var("DEVIN_BINARY") {
        spec.command.program = binary;
    }
    let adapter = Adapter::new(spec);
    // ACP streams turn text as MessageChunk events; TurnOutcome::text
    // intentionally stays empty. Collect the stream to prove the turn
    // produced agent output end to end.
    let text = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&text);
    let (events, mut inbox) = mpsc::unbounded_channel::<AgentEvent>();
    let drain = tokio::spawn(async move {
        while let Some(event) = inbox.recv().await {
            if let AgentEvent::MessageChunk { text: chunk } = event {
                sink.lock().unwrap().push_str(&chunk);
            }
        }
    });
    let session = adapter
        .open_session(SessionRequest::default(), events)
        .await
        .expect("devin acp session opens");
    assert!(!session.session_id().is_empty());
    let outcome = session
        .prompt(TurnRequest {
            text: "Reply with exactly one word: pong".to_owned(),
            ..Default::default()
        })
        .await
        .expect("prompt turn completes");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert!(
        text.lock().unwrap().to_lowercase().contains("pong"),
        "streamed: {}",
        text.lock().unwrap()
    );
    session.close().await.expect("session closes cleanly");
    drain.abort();
}
