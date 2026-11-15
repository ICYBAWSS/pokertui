//! Pure poker rules: cards, hand evaluation and the table state machine.
//!
//! Nothing in this module performs I/O, which keeps the rules exhaustively
//! testable and lets `poker verify` replay a hand without a network.

pub mod cards;
pub mod eval;
pub mod table;

pub use cards::{Card, DECK_LEN, deck_bytes, full_deck};
pub use eval::{HandScore, eval5, eval7, eval_holdem};
pub use table::{
    HandRecord, HandResult, Legal, Move, Phase, Seat, SeatId, SeatView, Street, Table, TableParams,
    TableView, verify_record,
};
