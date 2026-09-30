use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table,
    },
    Frame,
};

use crate::app::{App, Column, InputMode};
use crate::gcloud::LiveState;
use crate::profile::{Profile, SyncMode};

/// Lines above the table that show what gcloud holds now: one per part.
const LIVE_LINES: u16 = 2;

/// The two-line header labels, one pair per column.
const HEADER_LABELS: [(&str, &str); 3] = [
    ("Profile", ""),
    ("User Account", "Project"),
    ("ADC Account", "Quota Project"),
];

/// Display width of the auth-status suffix rendered after an account value in normal mode:
/// a space and a two-cell emoji (" \u{1F511}" / " \u{1F512}").
const LOCK_GLYPH_WIDTH: usize = 3;

/// Cells added to the table's content width so the whole-percent truncation in
/// `Constraint::Percentage` cannot clip the widest cell.
const TABLE_WIDTH_SLACK: usize = 4;

/// Content width of each column in cells: the widest of the two header lines and of every
/// profile row. Account columns include the lock glyph, which is part of the rendered cell.
/// Values are ASCII (enforced on input), so `len()` is the display width.
fn column_content_widths(profile_names: &[String], profiles: &[Profile]) -> [usize; 3] {
    let mut col_max = [0usize; 3];
    for (i, (line1, line2)) in HEADER_LABELS.iter().enumerate() {
        col_max[i] = line1.len().max(line2.len());
    }
    for (name, profile) in profile_names.iter().zip(profiles) {
        col_max[0] = col_max[0].max(name.len());
        col_max[1] = col_max[1]
            .max(profile.user_account.len() + LOCK_GLYPH_WIDTH)
            .max(profile.user_project.len());
        col_max[2] = col_max[2]
            .max(profile.adc_account.len() + LOCK_GLYPH_WIDTH)
            .max(profile.adc_quota_project.len());
    }
    col_max
}

/// The width constraints handed to `Table::new`. `column_rects` must receive this same array.
fn column_constraints(col_max: [usize; 3]) -> [Constraint; 3] {
    let total = col_max.iter().sum::<usize>().max(1);
    // w <= total, so w * 100 / total is at most 100 and fits in u16 after the division.
    col_max.map(|w| Constraint::Percentage((w * 100 / total).max(1) as u16))
}

/// The column rectangles exactly as `Table` lays them out inside `area`.
///
/// Mirrors `Table::get_column_widths` (ratatui-widgets 0.3.0): the constraints are solved on a
/// zero-origin rect of the table's width with the table's flex and column spacing, and every
/// cell is drawn at `area.x + x`. The table reserves a selection column of the highlight
/// symbol's width first; this app sets no highlight symbol, so that column is zero wide.
/// Anything the cursor or an overlay positions inside the table goes through here: a second
/// derivation of the same geometry is where the cursor drifts by a cell.
fn column_rects(area: Rect, widths: [Constraint; 3]) -> [Rect; 3] {
    let cols: [Rect; 3] = Layout::horizontal(widths)
        .flex(Flex::Start)
        .spacing(0u16)
        .areas(Rect::new(0, 0, area.width, 1));
    cols.map(|c| Rect::new(area.x + c.x, area.y, c.width, area.height))
}

/// Index into `column_rects` of the column whose cell is being edited.
fn edit_column_index(col: Column) -> usize {
    match col {
        Column::Adc => 2,
        // `Both` is mapped to `User` before edit mode is entered (the `e` key in `app.rs`).
        Column::User | Column::Both => 1,
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let frame_area = frame.area();

    // Always use normal mode help line width for stable layout
    let normal_help_width = build_normal_help_width(app);

    // Build the actual help line for the current mode
    let help_line = build_help_line(app);

    // Calculate table content width
    let table_width = table_content_width(app);

    let live_lines = build_live_lines(&app.live, app.live_user_valid, app.live_adc_valid);
    let live_width = live_lines.iter().map(Line::width).max().unwrap_or(0);

    // Minimum width: the widest of help, table and live lines, capped at terminal width
    let content_width = (normal_help_width.max(table_width).max(live_width) as u16)
        .min(frame_area.width);

    // Table height
    let table_h: u16 = if app.profile_names.is_empty() {
        1
    } else {
        2 + (app.profile_names.len() as u16) * 2
    };

    // Total content height: live lines + table + status bar + help
    let total_h = LIVE_LINES + table_h + 2;

    // Center horizontally; center vertically if content fits
    let x = (frame_area.width.saturating_sub(content_width)) / 2;
    let (y, height, table_constraint) = if total_h <= frame_area.height {
        let y = (frame_area.height - total_h) / 2;
        (y, total_h, Constraint::Length(table_h))
    } else {
        (0, frame_area.height, Constraint::Min(3))
    };

    let centered = Rect {
        x,
        y,
        width: content_width,
        height,
    };

    let chunks = Layout::vertical([
        Constraint::Length(LIVE_LINES),
        table_constraint,
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(centered);

    frame.render_widget(Paragraph::new(live_lines.to_vec()), chunks[0]);
    draw_table(frame, app, chunks[1]);
    draw_status_bar(frame, app, chunks[2]);
    frame.render_widget(Paragraph::new(help_line), chunks[3]);
    draw_suggestions(frame, app, chunks[1]);
}

/// The auth-status suffix after a credential: a key when it is valid, a lock when it is not,
/// nothing while unchecked.
fn lock_glyph(valid: Option<bool>) -> &'static str {
    match valid {
        Some(true) => " \u{1F511}",
        Some(false) => " \u{1F512}",
        None => "",
    }
}

/// One line per part: the label, what gcloud holds, any drift from the profiles, and the
/// validity of that credential.
fn build_live_lines(
    live: &LiveState,
    user_valid: Option<bool>,
    adc_valid: Option<bool>,
) -> [Line<'static>; 2] {
    let label = |text: &str| {
        Span::styled(
            format!("{:<8}", text),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )
    };
    let line = |text: &str, value: Option<(String, Option<String>)>, valid: Option<bool>| {
        let mut spans = vec![label(text)];
        match value {
            Some((description, note)) => {
                spans.push(Span::raw(description));
                if let Some(note) = note {
                    spans.push(Span::styled(
                        format!(" ({})", note),
                        Style::default().fg(Color::Yellow),
                    ));
                }
                spans.push(Span::raw(lock_glyph(valid).to_string()));
            }
            None => spans.push(Span::styled("none", Style::default().fg(Color::DarkGray))),
        }
        Line::from(spans)
    };
    [
        line(
            "gcloud",
            live.configuration.as_ref().map(|c| (c.describe(), c.drift_note())),
            user_valid,
        ),
        line(
            "ADC",
            live.adc.as_ref().map(|a| (a.describe(), a.drift_note())),
            adc_valid,
        ),
    ]
}

fn table_content_width(app: &App) -> usize {
    if app.profile_names.is_empty() {
        return 36;
    }
    column_content_widths(&app.profile_names, &app.profiles)
        .iter()
        .sum::<usize>()
        + TABLE_WIDTH_SLACK
}

fn draw_table(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.profile_names.is_empty() {
        let empty = Paragraph::new("  No profiles. Press 'n' to add one.")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, area);
        return;
    }

    let header_cells = HEADER_LABELS.iter().map(|(line1, line2)| {
        let style = Style::default()
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD);
        if line2.is_empty() {
            Cell::from(*line1).style(style)
        } else {
            Cell::from(format!("{}\n{}", line1, line2)).style(style)
        }
    });
    let header = Row::new(header_cells)
        .height(2)
        .style(Style::default().bg(Color::Indexed(254)));

    let rows = app
        .profile_names
        .iter()
        .zip(app.profiles.iter())
        .enumerate()
        .map(|(i, (name, profile))| {
            let user_active = app.active_profile.as_deref() == Some(name.as_str());
            let adc_active = app.active_adc.as_deref() == Some(name.as_str());
            let is_active = user_active || adc_active;
            let is_selected = i == app.selected_row;
            let profile_name = name.to_string();

            let is_editing = i == app.selected_row
                && matches!(app.input_mode, InputMode::EditAccount | InputMode::EditProject);

            let user_lock = lock_glyph(app.user_auth_valid.get(i).copied().flatten());
            let user_info = if is_editing && app.edit_col == Column::User {
                format!("{}\n{}", app.edit_account_buffer, app.edit_project_buffer)
            } else {
                format!("{}{}\n{}", profile.user_account, user_lock, profile.user_project)
            };

            let adc_lock = lock_glyph(app.adc_auth_valid.get(i).copied().flatten());
            let adc_info = if is_editing && app.edit_col == Column::Adc {
                format!("{}\n{}", app.edit_account_buffer, app.edit_project_buffer)
            } else {
                format!("{}{}\n{}", profile.adc_account, adc_lock, profile.adc_quota_project)
            };

            let light_grey       = Color::Indexed(255);
            let highlight_bg     = Color::Blue ;
            let col_highlight_bg = Color::Indexed(75); // lighter blue for selected column
            let edit_bg          = Color::Indexed(255); // Light Grey edit background

            let active_style = Style::default().bg(light_grey).fg(Color::Green).add_modifier(Modifier::BOLD);
            let plain_style  = Style::default().bg(light_grey).fg(Color::Black);

            // The row and its profile-name cell are active when either part is.
            let base_style = if is_selected {
                Style::default().bg(highlight_bg).fg(Color::White)
            } else if is_active {
                active_style
            } else {
                plain_style
            };

            // Each part cell shows its own active state: the user configuration and the ADC
            // can belong to different profiles.
            let col_style = |col: Column, active: bool, editing: bool| -> Style {
                if editing {
                    Style::default().bg(edit_bg).fg(Color::Black)
                } else if is_selected && app.selected_col == col {
                    Style::default().bg(col_highlight_bg).fg(Color::White).add_modifier(Modifier::BOLD)
                } else if is_selected {
                    base_style
                } else if active {
                    active_style
                } else {
                    plain_style
                }
            };

            let profile_style = base_style;
            let user_style    = col_style(Column::User, user_active, is_editing && app.edit_col == Column::User);
            let adc_style     = col_style(Column::Adc,  adc_active,  is_editing && app.edit_col == Column::Adc);

            Row::new(vec![
                Cell::from(profile_name).style(profile_style),
                Cell::from(user_info   ).style(user_style   ),
                Cell::from(adc_info    ).style(adc_style    ),
            ])
            .height(2).style(base_style)
        });

    // The same `widths` lay the table out and, below, place the cursor: one geometry.
    let widths = column_constraints(column_content_widths(&app.profile_names, &app.profiles));

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(0)
        .row_highlight_style(Style::default());

    frame.render_stateful_widget(table, area, &mut app.table_state);

    // Scrollbar for the table when rows overflow
    let header_height = 2u16;
    let row_height = 2u16;
    let visible_rows = area.height.saturating_sub(header_height) / row_height;
    let total_rows = app.profile_names.len();
    if total_rows as u16 > visible_rows {
        let max_offset = total_rows.saturating_sub(visible_rows as usize);
        let mut scrollbar_state = ScrollbarState::new(max_offset)
            .position(app.table_state.offset().min(max_offset));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray));
        frame.render_stateful_widget(
            scrollbar,
            area.inner(ratatui::layout::Margin { horizontal: 0, vertical: 1 }),
            &mut scrollbar_state,
        );
    }

    // Position the terminal cursor for blinking edit cursor
    if matches!(app.input_mode, InputMode::EditAccount | InputMode::EditProject) {
        let col = column_rects(area, widths)[edit_column_index(app.edit_col)];
        // Keep the caret inside its column when the buffer is wider than the clipped cell.
        let cursor_x = col.x + (app.edit_cursor_pos as u16).min(col.width.saturating_sub(1));
        let scroll_offset = app.table_state.offset();
        let cursor_y = area.y
            + 2  // header height
            + (app.selected_row.saturating_sub(scroll_offset) as u16) * 2
            + if app.input_mode == InputMode::EditProject { 1 } else { 0 };

        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let is_input_mode = matches!(
        app.input_mode,
        InputMode::AddProfileName
            | InputMode::AddProfileUserAccount
            | InputMode::AddProfileUserProject
            | InputMode::AddProfileAdcAccount
            | InputMode::AddProfileAdcQuotaProject
    );

    let line = if is_input_mode {
        let prompt = app.status_message.as_deref().unwrap_or("Input:");
        Line::from(vec![
            Span::styled(
                format!(" {} ", prompt),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                app.input_buffer.as_str().to_string(),
                Style::default().fg(Color::White),
            ),
            Span::styled("_", Style::default().fg(Color::Gray)),
        ])
    } else if let Some(ref msg) = app.status_message {
        Line::from(vec![
            Span::styled(
                format!(" {}", msg),
                Style::default().fg(Color::Green),
            ),
        ])
    } else {
        Line::default()
    };

    let bar = Paragraph::new(line);
    frame.render_widget(bar, area);
}

fn help_key(key: &str, desc: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            key.to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc.to_string(), Style::default().fg(Color::DarkGray)),
    ]
}

fn title_prefix() -> Vec<Span<'static>> {
    vec![
        Span::styled(
            "gcloud-switch",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
    ]
}

fn build_normal_help_spans(app: &App) -> Vec<Span<'static>> {
    let mut s = title_prefix();
    s.extend(help_key("\u{2191}\u{2193}", " row "));
    s.extend(help_key("\u{2190}\u{2192}", " col "));
    s.extend(help_key("\u{21b5}", " activate "));
    s.extend(help_key("a", "uthenticate "));
    s.extend(help_key("e", "dit "));
    s.extend(help_key("n", "ew "));
    s.extend(help_key("d", "el "));
    s.extend(help_key("s", "ync"));
    let sync_mode_label = match app.sync_mode {
        SyncMode::Strict => "(both)",
        SyncMode::Add => "(add)",
        SyncMode::Off => "(off)",
    };
    s.push(Span::styled(
        format!("{} ", sync_mode_label),
        Style::default().fg(Color::DarkGray),
    ));
    s.extend(help_key("i", "mport "));
    s.extend(help_key("esc", " exit"));
    s
}

fn build_normal_help_width(app: &App) -> usize {
    Line::from(build_normal_help_spans(app)).width()
}

fn build_help_line(app: &App) -> Line<'static> {
    let spans: Vec<Span> = match app.input_mode {
        InputMode::Normal => build_normal_help_spans(app),
        InputMode::ConfirmDelete => {
            let mut s = title_prefix();
            s.extend(help_key("y", "es "));
            s.extend(help_key("n", "/Esc cancel"));
            s
        }
        InputMode::EditAccount | InputMode::EditProject => {
            let mut s = title_prefix();
            s.extend(help_key("Tab", " next "));
            s.extend(help_key("\u{2193}", " suggestions "));
            s.extend(help_key("\u{23ce}", " save "));
            s.extend(help_key("Esc", " cancel"));
            s
        }
        _ => {
            let mut s = title_prefix();
            s.extend(help_key("\u{23ce}", "confirm"));
            s.extend(help_key("Esc", " cancel"));
            s
        }
    };
    Line::from(spans)
}

fn draw_suggestions(frame: &mut Frame, app: &App, table_area: Rect) {
    if app.suggestion_index.is_none() || app.suggestions.is_empty() {
        return;
    }

    let selected_idx = app.suggestion_index.unwrap_or(0);

    // The dropdown opens at the left edge of the edited column, where the table draws it.
    let widths = column_constraints(column_content_widths(&app.profile_names, &app.profiles));
    let dropdown_x = column_rects(table_area, widths)[edit_column_index(app.edit_col)].x;

    // Y position: header (2) + rows above * 2 + current row offset
    let row_y_offset = if app.input_mode == InputMode::EditAccount {
        1 // below the account line
    } else {
        2 // below the project line
    };
    let scroll_offset = app.table_state.offset();
    let dropdown_y = table_area.y + 2 + (app.selected_row.saturating_sub(scroll_offset) as u16) * 2 + row_y_offset;

    // Dropdown dimensions
    let max_item_width = app
        .suggestions
        .iter()
        .map(|s| s.len())
        .max()
        .unwrap_or(20) as u16;
    let dropdown_w = (max_item_width + 4).clamp(20, 50);
    let dropdown_h = (app.suggestions.len() as u16 + 2).min(12); // +2 for borders

    // Clamp to screen bounds
    let frame_area = frame.area();
    let dropdown_x = dropdown_x.min(frame_area.width.saturating_sub(dropdown_w));
    let dropdown_y = dropdown_y.min(frame_area.height.saturating_sub(dropdown_h));

    let dropdown_area = Rect {
        x: dropdown_x,
        y: dropdown_y,
        width: dropdown_w,
        height: dropdown_h,
    };

    // Clear the area behind the popup
    frame.render_widget(Clear, dropdown_area);

    let items: Vec<ListItem> = app
        .suggestions
        .iter()
        .enumerate()
        .map(|(i, suggestion)| {
            let style = if i == selected_idx {
                Style::default()
                    .bg(Color::Indexed(24))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(suggestion.as_str()).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );

    let mut list_state = ListState::default().with_selected(Some(selected_idx));
    frame.render_stateful_widget(list, dropdown_area, &mut list_state);

    // Scrollbar (only if items overflow the visible area)
    let visible_items = dropdown_area.height.saturating_sub(2) as usize; // minus borders
    if app.suggestions.len() > visible_items {
        let mut scrollbar_state = ScrollbarState::new(app.suggestions.len().saturating_sub(visible_items))
            .position(selected_idx.saturating_sub(visible_items / 2).min(app.suggestions.len().saturating_sub(visible_items)));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray));
        frame.render_stateful_widget(
            scrollbar,
            dropdown_area.inner(ratatui::layout::Margin { horizontal: 0, vertical: 1 }),
            &mut scrollbar_state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::widgets::{StatefulWidget, TableState};

    /// Content shapes whose `w * 100 / total` residues differ, so the whole-percent
    /// truncation lands on different cells.
    const SHAPES: [[usize; 3]; 3] = [
        [10, 23, 23], // the reported case: 17/41/41 %
        [7, 26, 26],  // header-bound profile column: 11/44/44 %
        [30, 12, 45], // profile column widest: 34/13/51 %
    ];

    /// Renders a real `Table` with the constraints `column_rects` receives and checks that
    /// the first character of every column lands at `rects[i].x`: for every table width
    /// from 30 to 220 cells, with and without an x offset, with a row selected.
    ///
    /// The reported defect: at 108 cells the table starts the User column at 18 while the
    /// former `w * width / total` put the cursor at 19.
    #[test]
    fn column_rects_match_where_the_table_draws() {
        const MARKERS: [&str; 3] = ["A", "B", "C"];
        for shape in SHAPES {
            for width in 30..=220u16 {
                for x in [0u16, 7] {
                    let area = Rect::new(x, 3, width, 6); // header (2) + two rows of 2
                    let widths = column_constraints(shape);
                    let rects = column_rects(area, widths);

                    let header = Row::new(
                        HEADER_LABELS.map(|(line1, line2)| Cell::from(format!("{line1}\n{line2}"))),
                    )
                    .height(2);
                    let rows = [
                        Row::new(MARKERS.map(Cell::from)).height(2),
                        Row::new(MARKERS.map(Cell::from)).height(2),
                    ];
                    let table = Table::new(rows, widths).header(header).column_spacing(0);
                    let mut state = TableState::default().with_selected(Some(0));
                    let mut buf = Buffer::empty(area);
                    StatefulWidget::render(table, area, &mut buf, &mut state);

                    let y = area.y + 2; // first data row, directly below the two-line header
                    for (i, (rect, marker)) in rects.iter().zip(MARKERS).enumerate() {
                        let drawn_at: Vec<u16> = (area.left()..area.right())
                            .filter(|&cx| buf[(cx, y)].symbol() == marker)
                            .collect();
                        let expected: Vec<u16> = if rect.width == 0 { vec![] } else { vec![rect.x] };
                        assert_eq!(
                            drawn_at, expected,
                            "shape {shape:?}, width {width}, x {x}: column {i} drawn at {drawn_at:?}, column_rects says {rect:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn column_constraints_stay_within_one_to_one_hundred_percent() {
        for shape in [[700usize, 700, 700], [1, 65_535, 65_535], [0, 0, 0]] {
            for constraint in column_constraints(shape) {
                match constraint {
                    Constraint::Percentage(p) => {
                        assert!((1..=100).contains(&p), "{shape:?} -> {constraint:?}");
                    }
                    other => panic!("{shape:?}: expected Percentage, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn column_content_widths_cover_headers_and_the_lock_glyph() {
        let names = vec!["p".to_string()];
        let profiles = vec![Profile {
            user_account: "a@b.c".into(),
            user_project: "x".into(),
            adc_account: "longer.name@example.com".into(),
            adc_quota_project: "q".into(),
            updated_at: None,
        }];
        // Profile and User columns are header-bound; ADC is bound by the account plus glyph.
        assert_eq!(column_content_widths(&names, &profiles), [7, 12, 23 + LOCK_GLYPH_WIDTH]);
    }
}
