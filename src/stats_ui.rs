//! The stats screen: tabs of numbers (each with its formula), line charts,
//! bar charts and a starting-hand heat map, all computed from `stats::HandStat`.

use crate::stats::{self, HandStat, POSITIONS};
use crate::ui::{BAD, DIM, GOLD, GREEN, TEXT};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Bar, BarChart, BarGroup, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Sparkline, Table, Tabs, Wrap};

const TABS: [&str; 3] = ["Summary", "Charts", "Starting hands"];

/// The lines the chart can draw: key, name, colour, what it shows.
const SERIES: [(char, &str, Color, &str); 6] = [
    ('t', "Total", Color::Rgb(235, 235, 230), "everything you've won or lost, as a running total (up = winning)"),
    ('s', "Showdown", Color::Rgb(100, 165, 235), "running total from hands that went to a showdown"),
    ('n', "No showdown", Color::Rgb(235, 120, 120), "running total from hands that ended before showdown: folds and bluffs"),
    ('w', "Win rate", Color::Rgb(235, 200, 90), "big blinds won per 100 hands, over a sliding window of recent hands"),
    ('v', "VPIP", Color::Rgb(120, 200, 220), "how often you played a hand, over a sliding window of recent hands"),
    ('p', "PFR", Color::Rgb(90, 190, 120), "how often you raised before the flop, over a sliding window of recent hands"),
];
pub const DEFAULT_LINES: [bool; 6] = [true, false, false, false, false, false];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Humans,
    Bots,
}

pub struct StatsScreen {
    pub hands: Vec<HandStat>,
    pub tab: usize,
    pub scroll: u16,
    pub filter: Filter,
    /// Show the long list with each formula instead of the cards.
    pub formulas: bool,
    /// Which chart lines are switched on, in `SERIES` order.
    pub lines: [bool; 6],
}

impl StatsScreen {
    pub fn load() -> Self {
        Self { hands: stats::load(), tab: 0, scroll: 0, filter: Filter::All, formulas: false, lines: DEFAULT_LINES }
    }

    fn selected(&self) -> Vec<&HandStat> {
        self.hands
            .iter()
            .filter(|h| match self.filter {
                Filter::All => true,
                Filter::Humans => h.bots == 0,
                Filter::Bots => h.bots > 0,
            })
            .collect()
    }

    /// Returns true when the player asked to leave the screen.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let tabs = TABS.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => return true,
            KeyCode::Right | KeyCode::Tab => self.set_tab((self.tab + 1) % tabs),
            KeyCode::Left | KeyCode::BackTab => self.set_tab((self.tab + tabs - 1) % tabs),
            KeyCode::Char(c @ '1'..='3') => self.set_tab(c as usize - '1' as usize),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Home => self.scroll = 0,
            KeyCode::Char(c) if self.tab == 1 && SERIES.iter().any(|x| x.0 == c.to_ascii_lowercase()) => {
                let i = SERIES.iter().position(|x| x.0 == c.to_ascii_lowercase()).unwrap_or(0);
                self.lines[i] = !self.lines[i];
            }
            KeyCode::Char('f') | KeyCode::Char('F') => {
                self.formulas = !self.formulas;
                self.scroll = 0;
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.filter = match self.filter {
                    Filter::All => Filter::Humans,
                    Filter::Humans => Filter::Bots,
                    Filter::Bots => Filter::All,
                };
                self.scroll = 0;
            }
            _ => {}
        }
        false
    }

    fn set_tab(&mut self, tab: usize) {
        self.tab = tab;
        self.scroll = 0;
    }
}

fn dim() -> Style {
    Style::default().fg(DIM)
}

fn signed(v: f64) -> Color {
    if v >= 0.0 { GREEN } else { BAD }
}

pub fn draw(f: &mut Frame, area: Rect, s: &StatsScreen) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    let hands = s.selected();

    // Header: title, filter and sample size, then the tabs.
    let filter = match s.filter {
        Filter::All => "all games",
        Filter::Humans => "no bots",
        Filter::Bots => "against bots",
    };
    let [title, tab_row] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(chunks[0]);
    f.render_widget(
        Line::from(vec![
            Span::styled(" YOUR STATS", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(format!("   {} hands · {filter}", hands.len()), dim()),
        ]),
        title,
    );
    // "[1] Summary" stays a clickable `[k]` label: the padding and divider keep 3+ spaces between tabs.
    let titles: Vec<Line> = TABS
        .iter()
        .enumerate()
        .map(|(i, name)| Line::from(vec![Span::styled(format!("[{}]", i + 1), Style::default().fg(GOLD)), Span::raw(format!(" {name}"))]))
        .collect();
    f.render_widget(
        Tabs::new(titles)
            .select(s.tab)
            .style(Style::default().fg(TEXT))
            .highlight_style(Style::default().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD))
            .padding(" ", " ")
            .divider("  "),
        tab_row,
    );

    let body = chunks[1];
    if hands.is_empty() {
        let msg = if s.hands.is_empty() {
            "No hands recorded yet. Play a few and they'll show up here."
        } else {
            "No hands match this filter. Press [b] to change it."
        };
        f.render_widget(Paragraph::new(Span::styled(msg, dim())).alignment(Alignment::Center), Rect::new(body.x, body.y + body.height / 2, body.width, 1));
    } else {
        match s.tab {
            0 if s.formulas => draw_overview(f, body, s, &hands),
            0 => draw_cards(f, body, &hands),
            1 => draw_charts(f, body, s, &hands),
            _ => draw_grid(f, body, &hands),
        }
    }

    f.render_widget(
        Paragraph::new(Span::styled(
            format!(" [b] games: {filter}   [f] {}   ←→ tab   ↑↓ scroll   [q] back", if s.formulas { "hide formulas" } else { "show formulas" }),
            dim(),
        )),
        chunks[2],
    );
}

/// Word-wrap `text` to `width`, indenting the first line with `first` and the rest with `rest`.
fn wrap(text: &str, width: usize, first: &str, rest: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut cur = first.to_string();
    let mut fresh = true;
    for word in text.split(' ') {
        if !fresh && cur.chars().count() + 1 + word.chars().count() > width.saturating_sub(1) {
            out.push(Line::from(Span::styled(std::mem::replace(&mut cur, rest.to_string()), dim())));
            fresh = true;
        }
        if !fresh {
            cur.push(' ');
        }
        cur.push_str(word);
        fresh = false;
    }
    out.push(Line::from(Span::styled(cur, dim())));
    out
}

fn scrolled(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>, scroll: u16) {
    let max = (lines.len() as u16).saturating_sub(area.height);
    let scroll = scroll.min(max);
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)).wrap(Wrap { trim: false }), area);
    if max > 0 {
        let mut state = ScrollbarState::new(max as usize).position(scroll as usize);
        f.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).style(dim()), area, &mut state);
    }
}

// ---------------------------------------------------------------- overview

/// The summary as cards: three headline numbers, then three small boxes.
fn draw_cards(f: &mut Frame, area: Rect, hands: &[&HandStat]) {
    let sum = stats::summarize(hands);
    let flat: Vec<stats::Row> = stats::rows(&sum).into_iter().flat_map(|g| g.1).collect();

    // Content of the three lower boxes, wrapped to the width they'll get.
    let inner_w = (area.width / 3).saturating_sub(2) as usize;
    let groups: [(&str, std::ops::Range<usize>); 3] = [("HIGHS AND LOWS", 3..5), ("HOW YOU PLAY", 5..8), ("SHOWDOWNS", 8..10)];
    // The "usual range" lines are the first thing to go when the screen is short.
    let build = |with_usual: bool| -> Vec<(&str, Vec<Line>)> {
        groups
            .iter()
            .map(|(title, range)| {
                let mut lines: Vec<Line> = Vec::new();
                for row in flat.iter().skip(range.start).take(range.len()) {
                    if !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {}  ", row.short), Style::default().fg(TEXT)),
                        Span::styled(row.value.clone(), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                    ]));
                    lines.extend(wrap(row.meaning, inner_w, " ", " "));
                    if with_usual && !row.typical.is_empty() {
                        lines.extend(wrap(&format!("usual: {}", row.typical), inner_w, " ", " "));
                    }
                }
                (*title, lines)
            })
            .collect()
    };
    let room = area.height.saturating_sub(5 + 1) as usize;
    let mut boxes = build(true);
    if boxes.iter().map(|b| b.1.len()).max().unwrap_or(0) + 2 > room {
        boxes = build(false);
    }
    let box_h = boxes.iter().map(|b| b.1.len()).max().unwrap_or(0) as u16 + 2;
    let rows = Layout::vertical([Constraint::Length(5), Constraint::Length(box_h), Constraint::Length(1), Constraint::Min(0)]).split(area);

    // Headline cards.
    let cols = Layout::horizontal([Constraint::Ratio(1, 3); 3]).split(rows[0]);
    for (i, row) in flat.iter().take(3).enumerate() {
        let colour = match i {
            0 | 1 if row.value.starts_with('-') => BAD,
            0 | 1 => GREEN,
            _ => GOLD,
        };
        let sub = match i {
            1 => format!("give or take {:.0}", sum.ci),
            2 => "50% = too early to say".to_string(),
            _ => row.meaning.to_string(),
        };
        let lines = vec![
            Line::from(Span::styled(row.value.clone(), Style::default().fg(colour).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled(sub, dim())),
        ];
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim())
            .title(Span::styled(format!(" {} ", row.short), Style::default().fg(GOLD)));
        let inner = block.inner(cols[i]);
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center).block(block), cols[i]);
        // The running total as a little trend line under the headline number.
        if i == 0 && inner.height >= 3 && inner.width >= 8 {
            let total = stats::cumulative(hands).0;
            let w = inner.width as usize - 4;
            let lo = total.iter().map(|p| p.1).fold(f64::MAX, f64::min);
            let data: Vec<u64> = (0..w)
                .map(|k| total[k * (total.len() - 1) / w.saturating_sub(1).max(1)].1)
                .map(|v| ((v - lo) * 100.0) as u64)
                .collect();
            let [_, _, line] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)]).areas(inner);
            let line = Rect { x: line.x + 2, width: line.width - 4, ..line };
            f.render_widget(Sparkline::default().data(&data).style(Style::default().fg(colour)), line);
        }
    }

    let cols = Layout::horizontal([Constraint::Ratio(1, 3); 3]).split(rows[1]);
    for (col, (title, lines)) in boxes.into_iter().enumerate() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim())
            .title(Span::styled(format!(" {title} "), Style::default().fg(GOLD)));
        f.render_widget(Paragraph::new(lines).block(block), cols[col]);
    }
    f.render_widget(
        Paragraph::new(Span::styled(" A few hundred hands say very little. Trust the give-or-take range, not the headline number.", dim())),
        rows[2],
    );
}

fn draw_overview(f: &mut Frame, area: Rect, s: &StatsScreen, hands: &[&HandStat]) {
    let sum = stats::summarize(hands);
    let mut lines: Vec<Line> = Vec::new();
    for (title, rows) in stats::rows(&sum) {
        lines.push(Line::from(Span::styled(format!(" {title}"), Style::default().fg(GOLD).add_modifier(Modifier::BOLD))));
        for r in rows {
            let colour = if r.name == "Total won or lost" || r.name == "Win rate" {
                if r.value.starts_with('-') { BAD } else { GREEN }
            } else {
                TEXT
            };
            let head = vec![
                Span::styled(format!("   {:<34}", r.name), Style::default().fg(TEXT)),
                Span::styled(format!("{:>24}", r.value), Style::default().fg(colour).add_modifier(Modifier::BOLD)),
            ];
            lines.push(Line::from(head));
            let note = if r.typical.is_empty() { r.formula.to_string() } else { format!("{}   ·   typical: {}", r.formula, r.typical) };
            lines.extend(wrap(&note, area.width as usize, "     = ", "       "));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        " A few hundred hands say very little: poker results swing wildly. Trust the ± range, not the headline number.",
        dim(),
    )));
    scrolled(f, area, lines, s.scroll);
}

// ---------------------------------------------------------------- graphs

fn bounds(series: &[&Vec<(f64, f64)>]) -> [f64; 2] {
    let vals = series.iter().flat_map(|s| s.iter().map(|p| p.1));
    let (lo, hi) = vals.fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v), b.max(v)));
    if lo > hi {
        return [-1.0, 1.0];
    }
    let pad = ((hi - lo) * 0.08).max(1.0);
    [lo - pad, hi + pad]
}

/// (hand number, value) for one chart line.
fn series_data(i: usize, hands: &[&HandStat], window: usize) -> Vec<(f64, f64)> {
    match i {
        0 => stats::cumulative(hands).0,
        1 => stats::cumulative(hands).1,
        2 => stats::cumulative(hands).2,
        3 => stats::rolling(hands, window, |h| 100.0 * h.net_bb()),
        4 => stats::rolling(hands, window, |h| if h.vpip { 100.0 } else { 0.0 }),
        _ => stats::rolling(hands, window, |h| if h.pfr { 100.0 } else { 0.0 }),
    }
}

fn draw_charts(f: &mut Frame, area: Rect, s: &StatsScreen, hands: &[&HandStat]) {
    if hands.len() < 3 {
        f.render_widget(Paragraph::new(Span::styled("Play a few more hands to draw charts.", dim())).alignment(Alignment::Center), area);
        return;
    }
    let seat_h = 10; // box + six seats
    let tall = area.height >= 22;
    let parts = if tall {
        Layout::vertical([Constraint::Length(2), Constraint::Min(7), Constraint::Length(2), Constraint::Length(seat_h)]).split(area)
    } else {
        Layout::vertical([Constraint::Length(2), Constraint::Min(7), Constraint::Length(2), Constraint::Length(0)]).split(area)
    };

    // Line picker: press the key (or click it) to switch a line on or off.
    let mut picker = vec![Span::raw(" ")];
    for (i, (key, name, colour, _)) in SERIES.iter().enumerate() {
        if i > 0 {
            picker.push(Span::raw("   "));
        }
        let on = s.lines[i];
        picker.push(Span::styled(format!("[{key}]"), Style::default().fg(GOLD)));
        picker.push(Span::styled(
            format!(" {} {name}", if on { "■" } else { "□" }),
            Style::default().fg(if on { *colour } else { DIM }),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(picker)).wrap(Wrap { trim: false }), parts[0]);

    let n = hands.len() as f64;
    let window = (hands.len() / 5).clamp(10, 200).min(hands.len());
    let on: Vec<usize> = (0..SERIES.len()).filter(|i| s.lines[*i]).collect();
    let block = Block::default().borders(Borders::ALL).border_style(dim()).title(Span::styled(" Your stats over time ", Style::default().fg(GOLD)));
    if on.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("Pick at least one line above.", dim())).alignment(Alignment::Center).block(block), parts[1]);
    } else {
        // One line shows real values; several lines are each stretched to their own
        // range so they can share a chart, and the legend gives the real numbers.
        let raw: Vec<Vec<(f64, f64)>> = on.iter().map(|i| series_data(*i, hands, window)).collect();
        let single = on.len() == 1;
        let range = |d: &Vec<(f64, f64)>| d.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
        let plotted: Vec<Vec<(f64, f64)>> = raw
            .iter()
            .map(|d| {
                if single {
                    d.clone()
                } else {
                    let (lo, hi) = range(d);
                    d.iter().map(|p| (p.0, if hi - lo < 1e-9 { 50.0 } else { (p.1 - lo) / (hi - lo) * 100.0 })).collect()
                }
            })
            .collect();
        let zero = [(0.0, 0.0), (n, 0.0)];
        let mut datasets = Vec::new();
        let y = if single {
            let y = bounds(&[&plotted[0]]);
            if y[0] < 0.0 && y[1] > 0.0 && (on[0] <= 3) {
                datasets.push(Dataset::default().marker(Marker::Dot).graph_type(GraphType::Line).style(dim()).data(&zero));
            }
            y
        } else {
            [0.0, 100.0]
        };
        for (k, data) in plotted.iter().enumerate() {
            datasets.push(
                Dataset::default().marker(Marker::HalfBlock).graph_type(GraphType::Line).style(Style::default().fg(SERIES[on[k]].2)).data(data),
            );
        }
        let unit = if single && on[0] >= 4 { "%" } else { "" };
        let lab = |v: f64| Span::styled(if single { format!("{v:.0}{unit}") } else { format!("{v:.0}%") }, dim());
        let ylabel = if single { if on[0] <= 2 { "big blinds" } else if on[0] == 3 { "bb per 100 hands" } else { "% of hands" } } else { "each line scaled to its own range" };
        f.render_widget(
            Chart::new(datasets)
                .block(block)
                .x_axis(Axis::default().title(Span::styled("hands played", dim())).style(dim()).bounds([0.0, n]).labels(vec![Span::styled("0", dim()), Span::styled(format!("{n:.0}"), dim())]))
                .y_axis(Axis::default().title(Span::styled(ylabel, dim())).style(dim()).bounds(y).labels(vec![lab(y[0]), lab((y[0] + y[1]) / 2.0), lab(y[1])])),
            parts[1],
        );

        // Legend: one plain sentence for a single line, real ranges for several.
        let legend: Vec<Span> = if single {
            vec![Span::styled(format!(" {}", SERIES[on[0]].3), dim())]
        } else {
            let mut sp = vec![Span::raw(" ")];
            for (k, d) in raw.iter().enumerate() {
                let (lo, hi) = range(d);
                let last = d.last().map(|p| p.1).unwrap_or(0.0);
                let u = if on[k] >= 4 { "%" } else { "" };
                sp.push(Span::styled("■ ", Style::default().fg(SERIES[on[k]].2)));
                sp.push(Span::styled(format!("{} {lo:.0}{u} to {hi:.0}{u}, now {last:.0}{u}    ", SERIES[on[k]].1), dim()));
            }
            sp
        };
        f.render_widget(Paragraph::new(Line::from(legend)).wrap(Wrap { trim: false }), parts[2]);
    }

    let bottom = tall.then_some(parts[3]);
    if let Some(area) = bottom {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim())
            .title(Span::styled(" Win rate by seat (bb/100) ", Style::default().fg(GOLD)));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let pos = stats::by_position(hands);
        let max = pos.iter().map(|p| p.1.abs()).fold(1.0, f64::max);
        let [hint, bars_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        f.render_widget(Paragraph::new(Span::styled(" Seats near the button act last and usually win most.", dim())), hint);
        let bars: Vec<Bar> = pos
            .iter()
            .enumerate()
            .map(|(i, (n, v))| {
                Bar::default()
                    .label(Line::from(format!(" {:<12}{:>4} hands ", POSITIONS[i], n)))
                    .value((v.abs() / max * 1000.0) as u64)
                    .text_value(format!("{v:+.0}"))
                    .style(Style::default().fg(signed(*v)))
                    .value_style(Style::default().fg(Color::Black).bg(signed(*v)))
            })
            .collect();
        f.render_widget(
            BarChart::default()
                .direction(Direction::Horizontal)
                .bar_width(1)
                .bar_gap(0)
                .max(1000)
                .label_style(Style::default().fg(TEXT))
                .data(BarGroup::default().bars(&bars)),
            bars_area,
        );
    }
}

// ---------------------------------------------------------------- starting hands

fn draw_grid(f: &mut Frame, area: Rect, hands: &[&HandStat]) {
    let g = stats::grid(hands);
    let avg = |c: (usize, f64)| if c.0 == 0 { 0.0 } else { c.1 / c.0 as f64 };
    let scale = g.iter().flatten().map(|c| avg(*c).abs()).fold(1.0, f64::max);
    let intro = " Every square is a starting hand: green made you money, red lost it, grey you haven't held yet. Pairs run down the middle, suited hands above it (s), offsuit below (o).";
    let intro_h = (intro.chars().count() as u16).div_ceil(area.width.max(1)) + 1;
    let [intro_area, table_area, rest_area] =
        Layout::vertical([Constraint::Length(intro_h), Constraint::Length(13), Constraint::Min(0)]).areas(area);
    f.render_widget(Paragraph::new(Span::styled(intro, dim())).wrap(Wrap { trim: false }), intro_area);
    let rows: Vec<Row> = (0..13)
        .map(|r| {
            Row::new((0..13).map(|c| {
                let cell = g[r][c];
                let style = if cell.0 == 0 {
                    Style::default().fg(Color::Rgb(95, 95, 95)).bg(Color::Rgb(28, 28, 28))
                } else {
                    let t = (avg(cell) / scale).clamp(-1.0, 1.0);
                    let (base, hue) = (30.0, if t >= 0.0 { (30.0, 150.0, 70.0) } else { (170.0, 45.0, 45.0) });
                    let k = t.abs().max(0.12);
                    let mix = |h: f64| (base + (h - base) * k) as u8;
                    Style::default().fg(Color::White).bg(Color::Rgb(mix(hue.0), mix(hue.1), mix(hue.2)))
                };
                Cell::from(format!(" {:<3}", stats::grid_label(r, c))).style(style)
            }))
        })
        .collect();
    let [_, table_area] = Layout::horizontal([Constraint::Length(1), Constraint::Min(0)]).areas(table_area);
    f.render_widget(Table::new(rows, [Constraint::Length(4); 13]).column_spacing(0), table_area);
    let mut lines: Vec<Line> = vec![Line::from("")];
    // Best and worst starting hands by total result.
    let mut played: Vec<(String, usize, f64)> = (0..13)
        .flat_map(|r| (0..13).map(move |c| (r, c)))
        .filter(|&(r, c)| g[r][c].0 >= 3)
        .map(|(r, c)| (stats::grid_label(r, c), g[r][c].0, g[r][c].1))
        .collect();
    played.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    let show = |title: &str, rows: Vec<&(String, usize, f64)>| -> Vec<Line<'static>> {
        let mut out = vec![Line::from(Span::styled(format!(" {title}"), Style::default().fg(GOLD).add_modifier(Modifier::BOLD)))];
        for (label, n, total) in rows {
            out.push(Line::from(vec![
                Span::styled(format!("   {label:<5}{n:>4} hands "), Style::default().fg(TEXT)),
                Span::styled(format!("{total:+8.1} bb  ({:+.1} per hand)", total / *n as f64), Style::default().fg(signed(*total))),
            ]));
        }
        out
    };
    if played.is_empty() {
        lines.push(Line::from(Span::styled(" Hold the same starting hand three times and it will be ranked here.", dim())));
    } else {
        lines.extend(show("YOUR BEST STARTING HANDS", played.iter().take(3).collect()));
        lines.push(Line::from(""));
        lines.extend(show("YOUR WORST STARTING HANDS", played.iter().rev().take(3).collect()));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rest_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn demo(n: usize) -> Vec<HandStat> {
        (0..n)
            .map(|i| HandStat {
                room: "r".into(),
                hand_no: i as u32,
                ts_ms: i as u64 * 60_000,
                bb: 20,
                players: 4,
                pos: (i % 6) as u8,
                hole: [(i % 52) as u8, ((i * 7 + 3) % 52) as u8],
                net: if i % 3 == 0 { 120 } else { -40 },
                invested: 40,
                pot: 100 + i as u64,
                vpip: i % 2 == 0,
                pfr: i % 4 == 0,
                reached: (i % 4) as u8,
                showdown: i % 5 == 0,
                cat: (i % 5 == 0).then_some((i % 9) as u8),
                aggr: 1,
                calls: 1,
                ..Default::default()
            })
            .collect()
    }

    /// Every tab draws at a normal and a cramped size without panicking, and the
    /// overview shows a formula.
    #[test]
    fn every_tab_draws() {
        for (w, h) in [(120, 40), (80, 24), (60, 18)] {
            for hands in [demo(0), demo(2), demo(300)] {
                let mut s = StatsScreen { hands, tab: 0, scroll: 0, filter: Filter::All, formulas: false, lines: DEFAULT_LINES };
                for tab in 0..TABS.len() {
                    for formulas in [false, true] {
                        s.tab = tab;
                        s.formulas = formulas;
                        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                        term.draw(|f| draw(f, f.area(), &s)).unwrap();
                    }
                }
            }
        }
        let s = StatsScreen { hands: demo(50), tab: 0, scroll: 0, filter: Filter::All, formulas: true, lines: DEFAULT_LINES };
        let mut term = Terminal::new(TestBackend::new(140, 60)).unwrap();
        term.draw(|f| draw(f, f.area(), &s)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("VPIP") && text.contains("÷ all hands"), "overview lacks stats or formulas");
    }

    #[test]
    fn keys_switch_tabs_and_filter() {
        let mut s = StatsScreen { hands: demo(5), tab: 0, scroll: 3, filter: Filter::All, formulas: false, lines: DEFAULT_LINES };
        let key = |c| KeyEvent::new(c, ratatui::crossterm::event::KeyModifiers::NONE);
        assert!(!s.handle_key(key(KeyCode::Char('3'))));
        assert_eq!((s.tab, s.scroll), (2, 0));
        s.handle_key(key(KeyCode::Char('b')));
        assert_eq!(s.filter, Filter::Humans);
        s.handle_key(key(KeyCode::Char('2')));
        s.handle_key(key(KeyCode::Char('w')));
        s.handle_key(key(KeyCode::Char('T')));
        assert_eq!(s.lines, [false, false, false, true, false, false], "w switches win rate on, t switches total off");
        s.handle_key(key(KeyCode::Char('1')));
        s.handle_key(key(KeyCode::Char('w')));
        assert!(s.lines[3], "line keys only act on the charts tab");
        assert!(s.handle_key(key(KeyCode::Esc)));
    }
}
