//! Drawing of the lazygit-style layout:
//!
//! ```text
//! ╭─[1] Query────────────────────────────────────────────╮
//! │❯ .users[] | select(.age > 30)                        │
//! ╰──────────────────────────────────────────────────────╯
//! ╭─[2] Suggestions─────╮╭─[4] Result────────────────────╮
//! │ .users              ││ {                             │
//! ╰─────────────────────╯│   "name": "Alice",            │
//! ╭─[3] History─────────╮│   ...                         │
//! │ .users[0]           ││                               │
//! ╰─────────────────────╯╰───────────────────────────────╯
//!  Run: enter | Complete: tab | ...              lazyjq 0.1.0
//! ```

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, Areas, Panel, Popup, Severity},
    config::{Config, Keys},
    suggestions::SuggestionKind,
};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let config = app.config.clone();
    let [main, bottom] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let [query, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(main);

    let (suggestions, history, result) = if frame.area().width < config.ui.portrait_threshold {
        let [result, side] =
            Layout::vertical([Constraint::Min(0), Constraint::Percentage(35)]).areas(body);
        let [suggestions, history] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(side);
        (suggestions, history, result)
    } else {
        let [side, result] = Layout::horizontal([
            Constraint::Percentage(config.ui.side_panel_width.min(80)),
            Constraint::Min(0),
        ])
        .areas(body);
        let [suggestions, history] =
            Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(side);
        (suggestions, history, result)
    };
    app.areas = Areas {
        query,
        suggestions,
        history,
        result,
    };

    draw_query(frame, app, query);
    draw_suggestions(frame, app, suggestions);
    draw_history(frame, app, history);
    draw_result(frame, app, result);
    draw_bottom_bar(frame, app, bottom);

    match &app.popup {
        Some(Popup::Help { .. }) => draw_help(frame, app),
        Some(Popup::Menu(_)) => draw_menu(frame, app),
        None => {}
    }
}

fn severity_style(config: &Config, severity: Severity) -> Style {
    match severity {
        Severity::Success => config.theme.success,
        Severity::Warning => config.theme.warning,
        Severity::Error => config.theme.error,
    }
}

fn is_active(app: &App, panel: Panel) -> bool {
    app.focus == panel && app.popup.is_none()
}

fn panel_block(app: &App, panel: Panel, extra: Option<Span<'static>>) -> Block<'static> {
    let config = &app.config;
    let active = is_active(app, panel);
    let border = if active {
        config.theme.active_border
    } else {
        config.theme.inactive_border
    };
    let title_style = if active {
        config.theme.active_border
    } else {
        config.theme.title
    };
    let mut title = vec![Span::styled(
        format!("[{}] {}", panel.number(), panel.title()),
        title_style,
    )];
    if let Some(extra) = extra {
        title.push(Span::raw(" "));
        title.push(extra);
    }
    Block::bordered()
        .border_type(config.ui.border.into())
        .border_style(border)
        .title(Line::from(title))
}

fn spinner(app: &App) -> &'static str {
    SPINNER[app.spinner % SPINNER.len()]
}

fn draw_query(frame: &mut Frame, app: &App, area: Rect) {
    let config = &app.config;
    let mut block = panel_block(app, Panel::Query, None);
    if let Some((message, severity)) = &app.query_status {
        let icon = if *severity == Severity::Error {
            "✗"
        } else {
            "!"
        };
        block = block.title_bottom(
            Line::styled(
                format!(" {icon} {} ", single_line(message)),
                severity_style(config, *severity),
            )
            .alignment(Alignment::Left),
        );
    }
    if app.evaluating {
        block = block.title(
            Line::styled(format!(" {} ", spinner(app)), config.theme.dim)
                .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prompt_width = config.editor.prompt.width();
    let text_width = usize::from(inner.width).saturating_sub(prompt_width);
    let (visible, cursor) = app.editor.viewport(text_width);
    let line = Line::from(vec![
        Span::styled(config.editor.prompt.clone(), config.editor.prompt_style),
        Span::raw(visible),
    ]);
    frame.render_widget(Paragraph::new(line), inner);

    if is_active(app, Panel::Query) && inner.width > 0 {
        let x = inner.x + (prompt_width + cursor).min(usize::from(inner.width) - 1) as u16;
        frame.set_cursor_position((x, inner.y));
    }
}

fn list_highlight(app: &App, panel: Panel) -> Style {
    if is_active(app, panel) {
        app.config.theme.selected_line
    } else {
        app.config.theme.inactive_selected_line
    }
}

fn counter(app: &App, selected: usize, len: usize, more: bool) -> Line<'static> {
    let more = if more { "+" } else { "" };
    let text = if len == 0 {
        String::new()
    } else {
        format!(" {} of {len}{more} ", selected + 1)
    };
    Line::styled(text, app.config.theme.dim).alignment(Alignment::Right)
}

fn draw_suggestions(frame: &mut Frame, app: &mut App, area: Rect) {
    let config = app.config.clone();
    let s = &app.suggestions;
    let kind = match s.kind {
        SuggestionKind::Paths => "paths",
        SuggestionKind::Values => "values",
    };
    let loading = s.kind == SuggestionKind::Paths && !app.paths.is_complete();
    let extra = if loading {
        format!("· {kind} {}", spinner(app))
    } else {
        format!("· {kind}")
    };
    let block = panel_block(
        app,
        Panel::Suggestions,
        Some(Span::styled(extra, config.theme.dim)),
    )
    .title_bottom(counter(app, s.selected, s.items.len(), s.truncated));

    if s.items.is_empty() {
        let note = s.note.clone().unwrap_or_default();
        frame.render_widget(
            Paragraph::new(Span::styled(note, config.theme.dim)).block(block),
            area,
        );
        app.suggestions_list.select(None);
        return;
    }

    let items: Vec<ListItem> = s
        .items
        .iter()
        .map(|item| ListItem::new(item.as_str()))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(list_highlight(app, Panel::Suggestions));
    app.suggestions_list.select(Some(app.suggestions.selected));
    frame.render_stateful_widget(list, area, &mut app.suggestions_list);
}

fn draw_history(frame: &mut Frame, app: &mut App, area: Rect) {
    let config = app.config.clone();
    let entries = app.history.entries();
    let block = panel_block(app, Panel::History, None).title_bottom(counter(
        app,
        app.history.selected,
        entries.len(),
        false,
    ));

    if entries.is_empty() {
        let hint = match config.keybinds.query.submit.display() {
            Some(key) => format!("Queries run with {key} show up here"),
            None => "No queries yet".into(),
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, config.theme.dim)).block(block),
            area,
        );
        app.history_list.select(None);
        return;
    }

    let items: Vec<ListItem> = entries.iter().map(|e| ListItem::new(e.as_str())).collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(list_highlight(app, Panel::History));
    app.history_list.select(Some(app.history.selected));
    frame.render_stateful_widget(list, area, &mut app.history_list);
}

fn draw_result(frame: &mut Frame, app: &mut App, area: Rect) {
    let config = app.config.clone();
    let tree = &app.result.tree;
    let count = tree.values().len();
    let extra = match count {
        1 => "· 1 value".to_string(),
        n => format!("· {n} values"),
    };
    let mut block = panel_block(
        app,
        Panel::Result,
        Some(Span::styled(extra, config.theme.dim)),
    );
    if app.query_status.is_some() {
        block = block.title_bottom(Line::styled(
            format!(
                " showing last valid result: {} ",
                single_line(&app.result.query)
            ),
            config.theme.warning,
        ));
    }
    if !tree.is_empty() {
        block = block.title_bottom(
            Line::styled(
                format!(" row {} of {} ", tree.cursor() + 1, tree.row_count()),
                config.theme.dim,
            )
            .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.empty_result {
        frame.render_widget(
            Paragraph::new(Span::styled("jq returned no results", config.theme.dim)),
            inner,
        );
        return;
    }

    let height = usize::from(inner.height);
    app.result.tree.scroll_into_view(height);
    let (lines, selected) = app
        .result
        .tree
        .render(height, &config.json, config.theme.dim);
    frame.render_widget(Paragraph::new(lines), inner);
    if let Some(line) = selected {
        let row = Rect {
            y: inner.y + line as u16,
            height: 1,
            ..inner
        };
        frame
            .buffer_mut()
            .set_style(row, list_highlight(app, Panel::Result));
    }
}

/// Hints `(keys, description)` for the focused panel.
fn hints(app: &App) -> Vec<(&Keys, &'static str)> {
    let k = &app.config.keybinds;
    match app.focus {
        Panel::Query => vec![
            (&k.query.submit, "run"),
            (&k.query.complete, "complete"),
            (&k.query.history_previous, "history"),
            (&k.query.leave, "result"),
            (&k.global.copy_query, "copy query"),
            (&k.global.help, "help"),
            (&k.global.force_quit, "quit"),
        ],
        Panel::Suggestions => vec![
            (&k.list.select, "use"),
            (&k.list.back, "back"),
            (&k.list.down, "next"),
            (&k.global.focus_query, "query"),
            (&k.global.help, "help"),
            (&k.global.quit, "quit"),
        ],
        Panel::History => vec![
            (&k.list.select, "run"),
            (&k.list.delete, "delete"),
            (&k.list.back, "back"),
            (&k.global.focus_query, "query"),
            (&k.global.help, "help"),
            (&k.global.quit, "quit"),
        ],
        Panel::Result => vec![
            (&k.result.toggle, "fold"),
            (&k.result.drill_down, "drill down"),
            (&k.result.copy_menu, "copy"),
            (&k.result.expand_all, "expand all"),
            (&k.result.collapse_all, "collapse all"),
            (&k.global.focus_query, "query"),
            (&k.global.help, "help"),
            (&k.global.quit, "quit"),
        ],
    }
}

fn draw_bottom_bar(frame: &mut Frame, app: &App, area: Rect) {
    let config = &app.config;
    let right = match &app.toast {
        Some(toast) => Span::styled(
            format!(" {} ", single_line(&toast.text)),
            severity_style(config, toast.severity),
        ),
        None => Span::styled(
            format!(" lazyjq {} ", env!("CARGO_PKG_VERSION")),
            config.theme.dim,
        ),
    };
    let right_width = (right.width() as u16).min(area.width);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
    frame.render_widget(Paragraph::new(right), right_area);

    if config.no_hint {
        return;
    }
    let mut spans = vec![Span::raw(" ")];
    for (keys, description) in hints(app) {
        let Some(key) = keys.display() else {
            continue;
        };
        if spans.len() > 1 {
            spans.push(Span::styled(" | ", config.theme.dim));
        }
        spans.push(Span::styled(
            format!("{description}: "),
            config.theme.hint_text,
        ));
        spans.push(Span::styled(key.to_lowercase(), config.theme.hint_key));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), left_area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn popup_block(app: &App, title: &'static str, footer: String) -> Block<'static> {
    Block::bordered()
        .border_type(app.config.ui.border.into())
        .border_style(app.config.theme.active_border)
        .title(Line::styled(title, app.config.theme.active_border))
        .title_bottom(Line::styled(footer, app.config.theme.dim).alignment(Alignment::Right))
}

fn help_lines(app: &App) -> Vec<Line<'static>> {
    let k = &app.config.keybinds;
    let sections: Vec<(&str, Vec<(&Keys, &str)>)> = vec![
        (
            "Global",
            vec![
                (&k.global.quit, "Quit (outside the query editor)"),
                (&k.global.force_quit, "Quit"),
                (&k.global.help, "Show this help"),
                (&k.global.copy_query, "Copy the query"),
                (&k.global.copy_result, "Copy the result"),
                (&k.global.focus_query, "Focus the query editor"),
                (&k.global.focus_suggestions, "Focus Suggestions"),
                (&k.global.focus_history, "Focus History"),
                (&k.global.focus_result, "Focus Result"),
                (&k.global.next_panel, "Next panel"),
                (&k.global.previous_panel, "Previous panel"),
            ],
        ),
        (
            "Query",
            vec![
                (
                    &k.query.submit,
                    "Run now, save to history and focus the result",
                ),
                (&k.query.leave, "Focus the result"),
                (&k.query.complete, "Complete from Suggestions"),
                (&k.query.history_previous, "Previous query in history"),
                (&k.query.history_next, "Next query in history"),
                (&k.query.move_to_head, "Move to the beginning"),
                (&k.query.move_to_tail, "Move to the end"),
                (&k.query.move_to_previous_word, "Previous word"),
                (&k.query.move_to_next_word, "Next word"),
                (&k.query.erase_all, "Clear the query"),
                (&k.query.erase_to_previous_word, "Delete the previous word"),
                (&k.query.erase_to_next_word, "Delete the next word"),
                (&k.query.erase_to_tail, "Delete to the end"),
            ],
        ),
        (
            "Suggestions / History",
            vec![
                (&k.list.up, "Previous item"),
                (&k.list.down, "Next item"),
                (&k.list.page_up, "Page up"),
                (&k.list.page_down, "Page down"),
                (&k.list.top, "First item"),
                (&k.list.bottom, "Last item"),
                (&k.list.select, "Put the item in the query"),
                (
                    &k.list.back,
                    "Back to the query (Suggestions: undo the completion)",
                ),
                (&k.list.delete, "Delete the history entry"),
            ],
        ),
        (
            "Result",
            vec![
                (&k.result.up, "Previous line"),
                (&k.result.down, "Next line"),
                (&k.result.page_up, "Half page up"),
                (&k.result.page_down, "Half page down"),
                (&k.result.top, "Top"),
                (&k.result.bottom, "Bottom"),
                (&k.result.toggle, "Fold/unfold"),
                (&k.result.expand, "Unfold / enter"),
                (&k.result.collapse, "Fold / go to parent"),
                (&k.result.expand_all, "Unfold everything"),
                (&k.result.collapse_all, "Fold everything"),
                (
                    &k.result.drill_down,
                    "Narrow the query to the selected node",
                ),
                (&k.result.copy_menu, "Copy menu"),
                (&k.result.back, "Back to the query"),
            ],
        ),
    ];

    let theme = &app.config.theme;
    let mut lines = Vec::new();
    for (title, bindings) in sections {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(
            title.to_string(),
            theme.active_border.add_modifier(Modifier::BOLD),
        ));
        for (keys, description) in bindings {
            let keys = keys.display_all();
            if keys.is_empty() {
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(format!("  {keys:<28}"), theme.hint_key),
                Span::raw(description.to_string()),
            ]));
        }
    }
    lines
}

fn draw_help(frame: &mut Frame, app: &mut App) {
    let lines = help_lines(app);
    let area = centered(frame.area(), 90, frame.area().height.saturating_sub(4));
    let visible = usize::from(area.height.saturating_sub(2));
    let max_scroll = lines.len().saturating_sub(visible);
    let scroll = match &mut app.popup {
        Some(Popup::Help { scroll }) => {
            *scroll = (*scroll).min(max_scroll);
            *scroll
        }
        _ => 0,
    };
    let close = app.config.keybinds.popup.close.display().unwrap_or("esc");
    let block = popup_block(
        app,
        " Keybindings ",
        format!(" {}: close ", close.to_lowercase()),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll as u16, 0)),
        area,
    );
}

fn draw_menu(frame: &mut Frame, app: &App) {
    let Some(Popup::Menu(menu)) = &app.popup else {
        return;
    };
    let lines: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let line = Line::from(vec![
                Span::styled(format!(" {} ", item.key), app.config.theme.hint_key),
                Span::raw(single_line(&item.label)),
            ]);
            if i == menu.selected {
                line.style(app.config.theme.selected_line)
            } else {
                line
            }
        })
        .collect();
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
    let area = centered(frame.area(), width.max(40), lines.len() as u16 + 2);
    let close = app.config.keybinds.popup.close.display().unwrap_or("esc");
    let block = popup_block(
        app,
        menu.title,
        format!(" {}: close ", close.to_lowercase()),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Messages are shown on one line.
fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}
