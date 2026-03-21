use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use super::app::App;
use super::widgets;

pub fn render(frame: &mut ratatui::Frame<'_>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let title = if app.input_mode {
        "Search (/ to edit, Enter to close input)"
    } else {
        "Search (/ to focus input)"
    };

    let query = Paragraph::new(app.query.value())
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(query, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(chunks[1]);

    let items: Vec<ListItem> = app
        .items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let scoped = item
                .scoped_id
                .clone()
                .unwrap_or_else(|| format!("{}/{}", item.publisher.handle, item.slug));
            let marker = if idx == app.selected { "> " } else { "  " };
            let line = format!("{}{} ({})", marker, item.name, scoped);
            ListItem::new(Line::from(Span::raw(line)))
        })
        .collect();

    let list_title = if app.loading {
        "Results (loading...)"
    } else {
        "Results"
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(list_title));
    frame.render_widget(list, body[0]);

    let (detail_title, detail_lines_raw) = if app.show_manifest {
        let scoped_id = app.selected_scoped_id();
        let lines = match scoped_id {
            None => vec!["No item selected".to_string()],
            Some(ref scoped) => {
                if let Some(content) = app.manifest_cache.get(scoped) {
                    content.lines().map(|line| line.to_string()).collect()
                } else if let Some(err) = app.manifest_errors.get(scoped) {
                    vec![
                        "capsule.toml unavailable".to_string(),
                        String::new(),
                        format!("reason: {}", err),
                    ]
                } else if app.manifest_inflight.contains(scoped) {
                    vec!["Loading capsule.toml ...".to_string()]
                } else {
                    vec!["Preparing capsule.toml ...".to_string()]
                }
            }
        };
        ("capsule.toml", lines)
    } else {
        ("Details", widgets::detail_lines(app.selected_item()))
    };
    let detail_lines = detail_lines_raw
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();

    let detail = Paragraph::new(detail_lines)
        .block(Block::default().borders(Borders::ALL).title(detail_title))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, body[1]);

    let mut footer = vec![Span::raw(
        "j/k or ↑/↓ move  / search  m manifest  Enter select  i install hint  q quit",
    )];
    if let Some(error) = &app.error {
        footer.push(Span::raw("  |  "));
        footer.push(Span::styled(
            format!("error: {}", error),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    } else if let Some(hint) = &app.hint {
        footer.push(Span::raw("  |  "));
        footer.push(Span::raw(hint.clone()));
    } else if let Some(cursor) = &app.next_cursor {
        footer.push(Span::raw("  |  "));
        footer.push(Span::raw(format!("next cursor: {}", cursor)));
    }

    let footer_widget = Paragraph::new(Line::from(footer));
    frame.render_widget(footer_widget, chunks[2]);
}
