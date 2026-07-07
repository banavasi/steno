use crate::app::App;
use crate::transcript::Speaker;
use chrono::Duration;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

fn speaker_style(sp: Speaker) -> Style {
    match sp {
        Speaker::Me => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        Speaker::Them => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(main);
    let [summary, claude] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(right);

    draw_transcript(f, app, left);
    draw_summary(f, app, summary);
    draw_claude(f, app, claude);
    draw_status(f, app, status);
}

fn draw_summary(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let mut sect = |title: &str, items: &[String]| {
        if items.is_empty() {
            return;
        }
        lines.push(Line::from(Span::styled(
            title.to_string(),
            Style::new().bold().fg(Color::Green),
        )));
        for i in items {
            lines.push(Line::from(format!("• {i}")));
        }
        lines.push(Line::raw(""));
    };
    sect("Summary", &app.summary.summary);
    sect("Decisions", &app.summary.decisions);
    sect("Action items", &app.summary.actions);
    sect("Points to discuss", &app.summary.points_to_discuss);
    sect("Open questions", &app.summary.open_questions);
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no summary yet)",
            Style::new().dim(),
        )));
    }
    let block = Block::new()
        .borders(Borders::ALL)
        .title(" Summary / Points ")
        .title_bottom(Line::from(format!(" {} ", app.summary_status)).right_aligned().dim());
    let inner = block.inner(area);
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    // keep the tail visible if the summary outgrows the pane
    let total = para.line_count(inner.width) as u16;
    let scroll = total.saturating_sub(inner.height);
    f.render_widget(para.block(block).scroll((scroll, 0)), area);
}

fn draw_claude(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == crate::app::Focus::Claude;
    let block = Block::new()
        .borders(Borders::ALL)
        .title(" Claude (haiku) ")
        .border_style(if focused {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new().dim()
        })
        .title_bottom(
            Line::from(if focused {
                " Ctrl+T → transcript · Ctrl+Q quit "
            } else {
                " Ctrl+T → chat "
            })
            .right_aligned()
            .dim(),
        );
    let inner = block.inner(area);
    match &mut app.claude {
        Some(pane) => {
            pane.ensure_size(inner.height, inner.width);
            let parser = pane.parser.clone();
            let guard = parser.lock().unwrap();
            let term = tui_term::widget::PseudoTerminal::new(guard.screen()).block(block);
            f.render_widget(term, area);
        }
        None => {
            f.render_widget(
                Paragraph::new("(claude unavailable — is the `claude` CLI on PATH?)")
                    .dim()
                    .block(block),
                area,
            );
        }
    }
}

fn draw_transcript(f: &mut Frame, app: &App, area: Rect) {
    let start = app.session.meeting.started;
    let mut lines: Vec<Line> = Vec::with_capacity(app.transcript.finals.len() + 2);
    for utt in &app.transcript.finals {
        let ts = (start + Duration::milliseconds((utt.t * 1000.0) as i64)).format("%H:%M");
        lines.push(Line::from(vec![
            Span::styled(format!("{ts} "), Style::new().dim()),
            Span::styled(utt.speaker.label(), speaker_style(utt.speaker)),
            Span::raw(" "),
            Span::raw(utt.text.clone()),
        ]));
    }
    for (sp, partial) in [
        (Speaker::Me, &app.transcript.partial_me),
        (Speaker::Them, &app.transcript.partial_them),
    ] {
        if let Some(text) = partial {
            lines.push(Line::from(vec![
                Span::styled("--:-- ", Style::new().dim()),
                Span::styled(sp.label(), speaker_style(sp).dim()),
                Span::raw(" "),
                Span::styled(format!("{text}▌"), Style::new().dim().italic()),
            ]));
        }
    }

    let mut block = Block::new().borders(Borders::ALL).title(format!(
        " Transcript — {} ({}) ",
        app.session.meeting.title,
        app.session.meeting.kind.label()
    ));
    if !app.has_loopback {
        // this must be LOUD: a meeting recorded mic-only looks fine until you
        // read the notes and half the conversation is missing
        block = block.title_bottom(
            Line::from(" ⚠ MIC-ONLY — participants are NOT captured (see README: loopback) ")
                .centered()
                .style(Style::new().fg(Color::Black).bg(Color::Yellow).bold()),
        );
    }
    let inner = block.inner(area);
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    // sticky tail: scroll so the last line is visible unless the user scrolled up
    let total = para.line_count(inner.width) as u16;
    let base = total.saturating_sub(inner.height);
    let scroll = base.saturating_sub(app.scroll_up.unwrap_or(0).min(base));
    f.render_widget(para.block(block).scroll((scroll, 0)), area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let elapsed = app.started.elapsed().as_secs();
    let rec = if app.all_paused {
        Span::styled(" ◼ PAUSED ", Style::new().fg(Color::Black).bg(Color::Yellow).bold())
    } else if app.mic_paused {
        Span::styled(" ◌ MIC PAUSED ", Style::new().fg(Color::Black).bg(Color::Yellow).bold())
    } else {
        Span::styled(" ● REC ", Style::new().fg(Color::White).bg(Color::Red).bold())
    };
    // -60..0 dB → 0..8 bars
    let level_bar = |db: f32| {
        let bars = ((db + 60.0) / 60.0 * 8.0).clamp(0.0, 8.0) as usize;
        format!("{}{}", "▮".repeat(bars), "▯".repeat(8 - bars))
    };
    let sys = if app.has_loopback {
        format!("  sys {}", level_bar(app.them_db))
    } else {
        "  sys ✗".into()
    };
    let mut spans = vec![
        rec,
        Span::raw(format!(
            " {:02}:{:02}:{:02}  mic {}{}  ",
            elapsed / 3600,
            (elapsed % 3600) / 60,
            elapsed % 60,
            level_bar(app.mic_db),
            sys,
        )),
    ];
    if app.headphones == Some(false) {
        spans.push(Span::styled(
            "⚠ speakers: Me/Them labels degraded  ",
            Style::new().fg(Color::Yellow),
        ));
    }
    if app.echo_suspect {
        spans.push(Span::styled(
            "⚠ echo detected  ",
            Style::new().fg(Color::Red).bold(),
        ));
    }
    spans.push(Span::styled(
        "[m]ic pause  [p]ause all  [Ctrl+T] chat  [↑↓] scroll  [Ctrl+Q] quit+save",
        Style::new().dim(),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
