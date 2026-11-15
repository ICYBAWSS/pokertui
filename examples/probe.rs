//! Transport gate: prove that rumqttc can speak MQTT over WSS to a public
//! broker, subscribe, publish, receive, and read a retained message.
//!
//!   cargo run --example probe
//!
//! Exits non-zero if any leg fails.

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("rumqttc=warn")
        .init();

    // NOTE: rumqttc takes host+port for websockets straight from `broker_addr`,
    // which must therefore be a full URL. The `port` argument is ignored for Ws/Wss.
    let broker = std::env::var("POKER_BROKER")
        .unwrap_or_else(|_| "wss://broker.emqx.io:8084/mqtt".into());
    let port: u16 = std::env::var("POKER_BROKER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8084);
    let room = std::env::var("POKER_PROBE_TOPIC")
        .unwrap_or_else(|_| format!("pokertui/probe/{}", std::process::id()));
    let topic = format!("{room}/events");

    let mk = |id: &str| {
        let mut opts = MqttOptions::new(id, broker.clone(), port);
        opts.set_keep_alive(Duration::from_secs(30));
        opts.set_transport(Transport::wss_with_default_config());
        opts
    };

    let (sub, mut sub_loop) = AsyncClient::new(mk(&format!("probe-sub-{}", std::process::id())), 32);
    sub.subscribe(&topic, QoS::AtLeastOnce).await?;

    let mut suback = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !suback {
        match tokio::time::timeout_at(deadline, sub_loop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::SubAck(_)))) => suback = true,
            Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => eprintln!("connected"),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => anyhow::bail!("sub eventloop error: {e}"),
            Err(_) => anyhow::bail!("timed out waiting for SUBACK (wss transport unusable)"),
        }
    }
    eprintln!("subscribed to {topic}");

    let (pubr, mut pub_loop) = AsyncClient::new(mk(&format!("probe-pub-{}", std::process::id())), 32);
    tokio::spawn(async move {
        loop {
            if let Err(e) = pub_loop.poll().await {
                eprintln!("pub eventloop error: {e}");
                break;
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    pubr.publish(&topic, QoS::AtLeastOnce, false, b"{\"kind\":\"probe\"}")
        .await?;

    let start = std::time::Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let got = loop {
        match tokio::time::timeout_at(deadline, sub_loop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::Publish(p)))) => break p,
            Ok(Ok(_)) => {}
            Ok(Err(e)) => anyhow::bail!("sub eventloop error: {e}"),
            Err(_) => anyhow::bail!("timed out waiting for PUBLISH"),
        }
    };
    eprintln!(
        "received {:?} bytes in {} ms",
        got.payload.len(),
        start.elapsed().as_millis()
    );

    // Retained message: publish, then subscribe from a brand new connection.
    pubr.publish(&topic, QoS::AtLeastOnce, true, b"{\"kind\":\"snapshot\"}")
        .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (late, mut late_loop) = AsyncClient::new(mk(&format!("probe-late-{}", std::process::id())), 32);
    late.subscribe(&topic, QoS::AtLeastOnce).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut retained = false;
    while !retained {
        match tokio::time::timeout_at(deadline, late_loop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::Publish(p)))) if p.retain => {
                eprintln!("retained snapshot delivered: {} bytes", p.payload.len());
                retained = true;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => anyhow::bail!("late eventloop error: {e}"),
            Err(_) => anyhow::bail!("timed out waiting for retained PUBLISH"),
        }
    }

    // Clear retained payload so repeated probes don't accumulate.
    pubr.publish(&topic, QoS::AtLeastOnce, true, b"").await?;
    println!("PROBE OK: {broker} (port hint {port}) pub/sub + retained works");
    Ok(())
}
