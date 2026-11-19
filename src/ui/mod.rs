//! Ratatui rendering: felt table, boxed seats, real-looking cards, the action
//! panel (your hand + big buttons) and the log.

use crate::app::{App, Mode};
use crate::game::Phase;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

mod card;
pub mod chips;
mod panel;
mod table;
mod overlay;
mod theme;
use card::{BIG, SMALL};
pub(crate) use theme::*;

pub const MIN_W: u16 = 80;
pub const MIN_H: u16 = 24;
/// Action panel height: room for BIG cards on tall terminals.
fn panel_h(height: u16) -> u16 {
    if height >= 28 { BIG.h + 2 } else { SMALL.h + 2 }
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width < MIN_W || area.height < MIN_H {
        let msg = Paragraph::new(vec![
            Line::from(format!("pokertui needs at least {MIN_W}×{MIN_H}")).style(Style::default().fg(GOLD)),
            Line::from(format!("this terminal is {}×{} · make it bigger", area.width, area.height)).style(Style::default().fg(DIM)),
        ])
        .alignment(Alignment::Center);
        f.render_widget(msg, popup(area, area.width, 2));
        return;
    }

    let panel = panel_h(area.height);
    // The log gets its own bordered box when there's room, a bare line otherwise.
    let spare = area.height.saturating_sub(1 + panel + 16);
    let log_h = if spare >= 4 { spare.min(8) } else { 1 };
    // Short terminals: the one-line log shares the footer row, so the table keeps room for the
    // pot line above the board.
    let merged = log_h == 1;
    let chunks = Layout::vertical([
        Constraint::Min(15),
        Constraint::Length(panel),
        Constraint::Length(if merged { 0 } else { log_h }),
        Constraint::Length(1),
    ])
    .split(area);

    draw_body(f, chunks[0], app);
    panel::draw_panel(f, chunks[1], app);
    if !merged {
        panel::draw_log(f, chunks[2], app);
    }
    panel::draw_footer(f, chunks[3], app, merged);

    if app.raise.is_some() {
        overlay::draw_raise(f, area, app);
    }
    if app.confirm_leave {
        overlay::draw_confirm_leave(f, area, app);
    }
    if app.bot_pick {
        overlay::draw_bot_pick(f, area);
    }
    if let Some(mv) = &app.confirm_move {
        overlay::draw_confirm_move(f, area, app, mv);
    }
    if app.help {
        overlay::draw_help(f, area);
    }
    if let Some((text, _)) = &app.notice {
        overlay::draw_notice(f, area, text);
    }
}

fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    // A client shows nothing from the table until the host has accepted it: before that, any
    // view is a stale retained snapshot of a host that may be long gone.
    let waiting_for_host = app.mode == Mode::Client && app.my_seat.is_none();
    let Some(view) = app.view.as_ref().filter(|_| !waiting_for_host) else {
        const SPIN: [&str; 4] = ["♠", "♥", "♦", "♣"];
        let spin = SPIN[(app.tick / 3 % 4) as usize];
        let dots = ".".repeat((app.tick / 6 % 4) as usize);
        let text = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{spin}  "), Style::default().fg(if matches!(spin, "♥" | "♦") { RED } else { TEXT })),
                Span::styled(
                    if app.connected {
                        format!("waiting for host{dots:<3}")
                    } else {
                        format!("connecting{dots:<3}")
                    },
                    Style::default().fg(GOLD),
                ),
            ]),
        ])
        .alignment(Alignment::Center);
        f.render_widget(text, popup(area, 64, 4));
        return;
    };

    if view.phase == Phase::Lobby {
        table::draw_lobby(f, area, app, view);
    } else {
        table::draw_table(f, area, app, view);
    }
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}


#[cfg(test)]
mod tests {
    use crate::app::{App, Mode};
    use crate::game::{Phase, Table, TableParams};
    use crate::net::{NetCommand, UiEvent};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    /// An App seated at a real 3-handed table, on the clock.
    fn app_on_turn() -> (App, tokio::sync::mpsc::UnboundedReceiver<NetCommand>) {
        let mut table = Table::new(TableParams { max_seats: 6, small_blind: 25, big_blind: 50, buyin: 5_000, turn_ms: 30_000, tourney: None });
        for name in ["alice", "bob", "carol"] {
            let seat = table.join(name, &format!("pk-{name}"), "enc", 5_000).unwrap();
            table.set_ready(seat, true);
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Mode::Client, "K7Q2-9XZP".into(), "wss://x".into(), tx, true);
        app.apply_event(UiEvent::View(table.view("r")));
        table.start_hand_seeded(1, [3; 32]).unwrap();
        table.set_deadline(u64::MAX / 2);
        let view = table.view("r");
        let me = view.actor.unwrap();
        app.apply_event(UiEvent::Joined { seat: me });
        app.apply_event(UiEvent::MyCards { hand: view.hand_no, cards: table.seats[me].as_ref().unwrap().hole.clone() });
        app.apply_event(UiEvent::View(view));
        assert_eq!(app.view.as_ref().unwrap().phase, Phase::Betting);
        (app, rx)
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn screen(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| super::draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A whole 3-handed hand, one App per seat: at every step exactly the
    /// player to act sees YOUR TURN and the buttons, everyone else sees who
    /// they're waiting for. Set POKER_DUMP=1 to print the screens.
    #[test]
    fn full_hand_only_the_actor_sees_controls() {
        let names = ["alice", "bob", "carol"];
        let mut table = Table::new(TableParams { max_seats: 6, small_blind: 25, big_blind: 50, buyin: 5_000, turn_ms: 30_000, tourney: None });
        for name in names {
            let seat = table.join(name, &format!("pk-{name}"), "enc", 5_000).unwrap();
            table.set_ready(seat, true);
        }
        let mut apps = Vec::new();
        let mut rxs = Vec::new();
        for seat in 0..names.len() {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(Mode::Client, "K7Q2-9XZP".into(), "wss://x".into(), tx, false);
            app.apply_event(UiEvent::Joined { seat });
            app.apply_event(UiEvent::View(table.view("r")));
            apps.push(app);
            rxs.push(rx);
        }
        table.start_hand_seeded(1, [7; 32]).unwrap();
        let dump = std::env::var("POKER_DUMP").is_ok();
        let mut steps = 0;
        loop {
            table.set_deadline(u64::MAX / 2);
            let view = table.view("r");
            for (seat, app) in apps.iter_mut().enumerate() {
                app.apply_event(UiEvent::MyCards { hand: view.hand_no, cards: table.seats[seat].as_ref().unwrap().hole.clone() });
                app.apply_event(UiEvent::View(view.clone()));
                app.anim = Default::default(); // render the settled frame
            }
            if view.phase != Phase::Betting {
                break;
            }
            let actor = view.actor.expect("someone acts while betting");
            for (seat, app) in apps.iter().enumerate().flat_map(|a| [(80, 24), (130, 42)].map(move |sz| (a, sz))).map(|((seat, app), (w, h))| (seat, (app, w, h))) {
                let (app, w, h) = app;
                let text = screen(app, w, h);
                if dump && seat == actor {
                    println!("--- step {steps}: {} to act ---\n{text}", names[seat]);
                }
                if seat == actor {
                    assert!(text.contains("Your turn") && text.contains("[f] Fold"), "step {steps}: actor sees no controls\n{text}");
                    assert!(text.contains("You have"), "step {steps}: actor is not told their hand\n{text}");
                } else {
                    assert!(!text.contains("Your turn"), "step {steps}: seat {seat} wrongly sees controls");
                    assert!(text.contains("'s turn"), "step {steps}: seat {seat} doesn't say whose turn\n{text}");
                }
            }
            assert!(apps[actor].bell, "turn start rings the bell");
            apps[actor].bell = false;
            apps[actor].handle_key(key(KeyCode::Char('c')));
            let Ok(NetCommand::Action(mv)) = rxs[actor].try_recv() else { panic!("key 2 sent no action") };
            table.apply(actor, mv, 2).unwrap();
            steps += 1;
            assert!(steps < 50, "hand never finished");
        }
        assert_eq!(table.view("r").phase, Phase::HandOver);
        for app in &apps {
            let text = screen(app, 80, 24);
            if dump {
                println!("--- hand over ---\n{text}");
            }
            assert!(text.contains("wins") || text.contains("YOU WIN"), "no winner banner\n{text}");
        }
    }

    #[test]
    fn raise_presets_are_legal_and_picker_clamps() {
        let (mut app, mut rx) = app_on_turn();
        let legal = app.my_legal().unwrap();
        let presets = app.raise_presets().unwrap();
        for (_, v) in presets {
            assert!((legal.min_raise_to..=legal.max_raise_to).contains(&v));
        }
        assert!(presets[1].1 <= presets[2].1, "half pot <= pot");
        // Open the picker, type an over-large amount, confirm: clamps to max.
        app.handle_key(key(KeyCode::Char('r')));
        assert!(app.raise.is_some());
        for c in "999999".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.raise.is_none());
        match rx.try_recv() {
            Ok(NetCommand::Action(crate::game::Move::Raise { to })) => assert_eq!(to, legal.max_raise_to),
            other => panic!("expected a raise, got {other:?}"),
        }
        // Key 2 calls when there's a bet to call.
        app.handle_key(key(KeyCode::Char('c')));
        assert!(matches!(rx.try_recv(), Ok(NetCommand::Action(crate::game::Move::Call))));
    }

    #[test]
    fn esc_asks_before_leaving() {
        let (mut app, _rx) = app_on_turn();
        app.handle_key(key(KeyCode::Esc));
        assert!(app.confirm_leave && !app.leave);
        app.handle_key(key(KeyCode::Char('n')));
        assert!(!app.confirm_leave && !app.leave);
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Char('y')));
        assert!(app.leave);
    }

    #[test]
    fn table_and_overlays_render_at_common_sizes() {
        for (w, h) in [(80, 24), (120, 40), (200, 60)] {
            let (mut app, _rx) = app_on_turn();
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| super::draw(f, &app)).unwrap();
            let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            assert!(text.contains("Your turn"), "{w}x{h}: no turn banner");
            assert!(text.contains("[f] Fold"), "{w}x{h}: no options");
            assert!(text.contains("You have"), "{w}x{h}: no hand description");
            app.handle_key(key(KeyCode::Char('r')));
            term.draw(|f| super::draw(f, &app)).unwrap();
            app.raise = None;
            app.confirm_leave = true;
            term.draw(|f| super::draw(f, &app)).unwrap();
            app.confirm_leave = false;
            app.help = true;
            term.draw(|f| super::draw(f, &app)).unwrap();
        }
    }
}
