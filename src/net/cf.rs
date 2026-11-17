//! Relay backend for the optional Cloudflare Durable Object relay
//! (`relay/cloudflare`). One WebSocket per room carries small JSON frames:
//!
//!   out: {"op":"sub","topic":T} | {"op":"pub","topic":T,"data":B64,"retain":bool}
//!   in:  {"op":"msg","topic":T,"data":B64,"retain":bool}
//!
//! Like the MQTT link it reconnects on its own and resubscribes every time.

use super::mqtt::Message;
use anyhow::{Result, bail};
use async_tungstenite::tokio::connect_async;
use async_tungstenite::tungstenite::Message as Ws;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// `cf+wss://host` → `wss://host`; anything else is not a Cloudflare relay.
pub fn ws_base(broker: &str) -> Option<String> {
    broker
        .strip_prefix("cf+")
        .filter(|rest| rest.starts_with("ws://") || rest.starts_with("wss://"))
        .map(|rest| rest.trim_end_matches('/').to_string())
}

pub struct CfConn {
    pub out: mpsc::UnboundedSender<String>,
}

pub fn sub_frame(topic: &str) -> String {
    serde_json::json!({ "op": "sub", "topic": topic }).to_string()
}

pub fn pub_frame(topic: &str, payload: &[u8], retain: bool) -> String {
    serde_json::json!({ "op": "pub", "topic": topic, "data": B64.encode(payload), "retain": retain }).to_string()
}

fn parse_in(text: &str) -> Option<Message> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("op")?.as_str()? != "msg" {
        return None;
    }
    Some(Message {
        topic: v.get("topic")?.as_str()?.to_string(),
        payload: B64.decode(v.get("data")?.as_str()?).ok()?,
        retain: v.get("retain").and_then(|r| r.as_bool()).unwrap_or(false),
    })
}

pub async fn connect(
    base: &str,
    room: &str,
    subscriptions: Arc<Mutex<Vec<String>>>,
    connected: Arc<AtomicBool>,
) -> Result<(CfConn, mpsc::UnboundedReceiver<Message>)> {
    let url = format!("{base}/room/{room}");
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<std::result::Result<(), String>>();

    tokio::spawn(async move {
        let mut ready = Some(ready_tx);
        loop {
            match connect_async(url.as_str()).await {
                Ok((ws, _)) => {
                    let (mut sink, mut stream) = ws.split();
                    connected.store(true, Ordering::SeqCst);
                    let subs: Vec<String> = subscriptions.lock().map(|s| s.clone()).unwrap_or_default();
                    for topic in subs {
                        let _ = sink.send(Ws::Text(sub_frame(&topic).into())).await;
                    }
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(Ok(()));
                    }
                    loop {
                        tokio::select! {
                            frame = out_rx.recv() => match frame {
                                Some(f) => {
                                    if sink.send(Ws::Text(f.into())).await.is_err() { break; }
                                }
                                None => return,
                            },
                            incoming = stream.next() => match incoming {
                                Some(Ok(Ws::Text(t))) => {
                                    if let Some(m) = parse_in(t.as_str()) {
                                        let _ = in_tx.send(m);
                                    }
                                }
                                Some(Ok(Ws::Ping(p))) => { let _ = sink.send(Ws::Pong(p)).await; }
                                Some(Ok(_)) => {}
                                _ => break,
                            },
                        }
                    }
                    connected.store(false, Ordering::SeqCst);
                    tracing::warn!("cloudflare relay connection lost; retrying");
                }
                Err(e) => {
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    match tokio::time::timeout(Duration::from_secs(10), ready_rx).await {
        Ok(Ok(Ok(()))) => Ok((CfConn { out: out_tx }, in_rx)),
        Ok(Ok(Err(e))) => bail!("{e}"),
        Ok(Err(_)) => bail!("relay connection task exited"),
        Err(_) => bail!("timed out connecting"),
    }
}
