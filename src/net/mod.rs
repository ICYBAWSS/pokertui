//! Networked play over a public pub/sub relay.
//!
//! The host is the only authority: it deals, validates, and broadcasts signed
//! table snapshots. Clients verify every signature, apply only newer versions,
//! and decrypt their own hole cards from a sealed per-player topic.

pub mod client;
pub mod host;
pub mod mqtt;
pub mod protocol;
pub mod roomcode;

pub use client::{ClientConfig, run_client};
pub use host::{HostConfig, run_host};
pub use mqtt::{Link, Message};
pub use protocol::{Envelope, Payload, Topics};
pub use roomcode::RoomId;

use crate::game::{Card, HandResult, Move, TableView};

pub type UiSender = std::sync::mpsc::Sender<UiEvent>;

/// Events flowing from the network side to the UI.
#[derive(Debug, Clone)]
pub enum UiEvent {
    Connected { broker: String },
    Disconnected(String),
    Joined { seat: usize },
    View(TableView),
    MyCards { hand: u32, cards: Vec<Card> },
    HandResult(HandResult),
    Chat { from: String, text: String },
    Info(String),
    Error(String),
    Closed(String),
}

/// Commands flowing from the UI to the network side.
#[derive(Debug, Clone)]
pub enum NetCommand {
    Action(Move),
    Chat(String),
    Start,
    Ready(bool),
    Leave,
    Quit,
}

pub fn sanitize_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}
