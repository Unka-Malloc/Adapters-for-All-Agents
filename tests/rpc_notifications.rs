//! Ordered notifications and EOF must not overtake a slow event consumer.
use a4aa::{JsonRpcPeer, RpcDelegate};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;

#[derive(Default)]
struct Events {
    seen: Mutex<Vec<String>>,
    entered: Notify,
    release: Notify,
    closed: Notify,
}
#[async_trait::async_trait]
impl RpcDelegate for Events {
    async fn on_notification(&self, method: &str, _params: Value) {
        if method == "first" {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.seen.lock().unwrap().push(method.to_owned());
    }
    async fn on_closed(&self) {
        self.seen.lock().unwrap().push("closed".into());
        self.closed.notify_one();
    }
}
#[tokio::test]
async fn notifications_and_eof_keep_wire_order() {
    let (host, mut agent) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(host);
    let events = Arc::new(Events::default());
    let peer = JsonRpcPeer::spawn(reader, writer, events.clone(), None, 4096);
    agent
        .write_all(b"{\"method\":\"first\"}\n{\"method\":\"second\"}\n")
        .await
        .unwrap();
    agent.shutdown().await.unwrap();
    events.entered.notified().await;
    assert!(events.seen.lock().unwrap().is_empty());
    events.release.notify_one();
    events.closed.notified().await;
    assert_eq!(
        *events.seen.lock().unwrap(),
        vec!["first", "second", "closed"]
    );
    peer.close().await;
}
