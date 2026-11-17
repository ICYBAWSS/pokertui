//! Networked play over a public pub/sub relay.
//!
//! The host is the only authority: it deals, validates, and broadcasts signed
//! table snapshots. Clients verify every signature, apply only newer versions,
//! and decrypt their own hole cards from a sealed per-player topic.

pub mod cf;
pub mod client;
pub mod host;
pub mod mqtt;
pub mod protocol;
pub mod roomcode;

pub use client::{ClientConfig, run_client, run_client_on};
pub use host::{HostConfig, run_host, run_host_on};
pub use mqtt::{Link, MemoryBus, Message};
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
    /// This client took over as host after the old host vanished.
    Promoted,
    /// A finished hand that every human player has co-signed (for the leaderboard).
    Bundle(crate::leaderboard::Bundle),
    /// Players waiting for the host to let them in (names, oldest first).
    Waiting(Vec<String>),
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
    /// Host only: remove a seat, optionally banning its key.
    Kick { seat: usize, ban: bool },
    AddBot(crate::bot::Skill),
    RemoveBot(usize),
    /// Host only: accept or decline the oldest player waiting to join.
    Approve(bool),
}

pub fn sanitize_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}
