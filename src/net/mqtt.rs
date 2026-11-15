//! MQTT-over-WebSocket transport.
//!
//! Everything is outbound TLS on 443-style WebSocket endpoints, so no port
//! forwarding, no NAT traversal and no relay of our own. The public brokers are
//! best-effort: `rumqttc`'s event loop reconnects, and we resubscribe on every
//! `CONNACK` because public brokers do not keep clean-session subscriptions.

use anyhow::{Context, Result, bail};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, Packet, QoS, Transport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

pub const DEFAULT_BROKERS: &[&str] = &[
    "wss://broker.emqx.io:8084/mqtt",
    "wss://test.mosquitto.org:8081/mqtt",
    "wss://broker.hivemq.com:8884/mqtt",
];

#[derive(Clone, Debug)]
pub struct Message {
    pub topic: String,
    pub payload: Vec<u8>,
    pub retain: bool,
}

pub struct Link {
    client: AsyncClient,
    rx: mpsc::UnboundedReceiver<Message>,
    subscriptions: Arc<Mutex<Vec<String>>>,
    pub connected: Arc<AtomicBool>,
    pub broker: String,
}

impl Link {
    pub async fn connect(broker: Option<&str>) -> Result<Link> {
        let candidates: Vec<String> = match broker {
            Some(url) => vec![url.to_string()],
            None => DEFAULT_BROKERS.iter().map(|s| s.to_string()).collect(),
        };
        let mut errors = Vec::new();
        for url in candidates {
            match Self::try_connect(&url).await {
                Ok(link) => {
                    tracing::info!(broker = %link.broker, "connected to relay");
                    return Ok(link);
                }
                Err(e) => {
                    tracing::warn!(broker = %url, error = %e, "relay unavailable");
                    errors.push(format!("{url}: {e}"));
                }
            }
        }
        bail!("could not reach any relay:\n  {}", errors.join("\n  "))
    }

    async fn try_connect(url: &str) -> Result<Link> {
        let client_id = format!("pokertui-{:08x}", std::process::id() ^ rand_u32());
        let mut opts = MqttOptions::new(client_id, url, 8084);
        opts.set_keep_alive(Duration::from_secs(20));
        opts.set_clean_session(true);
        opts.set_transport(if url.starts_with("ws://") {
            Transport::ws()
        } else {
            Transport::wss_with_default_config()
        });

        let (client, mut eventloop) = AsyncClient::new(opts, 128);
        let (tx, rx) = mpsc::unbounded_channel();
        let subscriptions: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connected = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

        {
            let subscriptions = subscriptions.clone();
            let connected = connected.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let mut ready = Some(ready_tx);
                loop {
                    match eventloop.poll().await {
                        Ok(Event::Incoming(Packet::ConnAck(_))) => {
                            connected.store(true, Ordering::SeqCst);
                            // Public brokers do not persist subscriptions.
                            let subs: Vec<String> =
                                subscriptions.lock().map(|s| s.clone()).unwrap_or_default();
                            for topic in subs {
                                let _ = client.subscribe(topic, QoS::AtLeastOnce).await;
                            }
                            if let Some(tx) = ready.take() {
                                let _ = tx.send(());
                            }
                        }
                        Ok(Event::Incoming(Packet::Publish(p))) => {
                            let _ = tx.send(Message {
                                topic: p.topic,
                                payload: p.payload.to_vec(),
                                retain: p.retain,
                            });
                        }
                        Ok(Event::Incoming(Packet::Disconnect)) | Ok(Event::Outgoing(_)) => {}
                        Ok(_) => {}
                        Err(e) => {
                            connected.store(false, Ordering::SeqCst);
                            tracing::warn!(error = %e, "relay connection lost; retrying");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            });
        }

        match tokio::time::timeout(Duration::from_secs(10), ready_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => bail!("relay connection task exited"),
            Err(_) => bail!("timed out connecting"),
        }

        Ok(Link {
            client,
            rx,
            subscriptions,
            connected,
            broker: url.to_string(),
        })
    }

    pub async fn subscribe(&self, topic: &str) -> Result<()> {
        if let Ok(mut subs) = self.subscriptions.lock()
            && !subs.iter().any(|s| s == topic)
        {
            subs.push(topic.to_string());
        }
        self.client
            .subscribe(topic, QoS::AtLeastOnce)
            .await
            .context("subscribe failed")
    }

    pub async fn publish(&self, topic: &str, payload: Vec<u8>, retain: bool) -> Result<()> {
        if !self.connected.load(Ordering::SeqCst) {
            bail!("not connected to the relay");
        }
        self.client
            .publish(topic, QoS::AtLeastOnce, retain, payload)
            .await
            .context("publish failed")
    }

    pub async fn recv(&mut self) -> Option<Message> {
        self.rx.recv().await
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

fn rand_u32() -> u32 {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    u32::from_le_bytes(b)
}

/// Never let a stray `Incoming` import warning mask real breakage.
#[allow(dead_code)]
fn _assert_incoming(_: Option<Incoming>) {}
