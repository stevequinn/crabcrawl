// TODO: Add pause unpause option to scraping.
//
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseEvent, MouseEventKind,
}; // Added Mouse types
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use fantoccini::{Client, Locator};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use url::Url;

// --- Constants ---
const WEBDRIVER_URL: &str = "http://localhost:4444";
const CRAWLER_CHANNEL_BUFFER: usize = 100;
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(100);
const SCROLL_LINES: u16 = 3; // Adjusted scroll speed slightly

// --- Application State ---

struct AppState {
    visited_urls: Vec<String>,
    body_texts: HashMap<String, String>,
    list_state: ListState,
    search_input: String,
    active_search_query: String,
    is_searching: bool,
    filtered_url_indices: Vec<usize>,
    content_scroll: u16,
    content_area: Rect, // Store the area/bounds of the content panel
}

impl AppState {
    fn new() -> Self {
        AppState {
            visited_urls: Vec::new(),
            body_texts: HashMap::new(),
            list_state: ListState::default(),
            search_input: String::new(),
            active_search_query: String::new(),
            is_searching: false,
            filtered_url_indices: Vec::new(),
            content_scroll: 0,
            content_area: Rect::default(), // Initialize with a default
        }
    }

    // --- Methods for adding/updating/getting data (mostly unchanged) ---
    fn add_crawl_result(&mut self, url: String, body: String) {
        if !self.body_texts.contains_key(&url) {
            let is_first_item = self.visited_urls.is_empty();
            self.visited_urls.push(url.clone());
            self.body_texts.insert(url, body);
            self.update_filtered_list();
            if is_first_item && !self.filtered_url_indices.is_empty() {
                self.list_state.select(Some(0));
                self.reset_or_find_scroll();
            }
        }
    }

    fn update_filtered_list(&mut self) {
        let query = self.active_search_query.to_lowercase();
        let previously_selected_original_index = self.get_selected_original_index();

        self.filtered_url_indices = self
            .visited_urls
            .iter()
            .enumerate()
            .filter(|(_idx, url)| {
                if query.is_empty() {
                    true
                } else {
                    self.body_texts
                        .get(*url)
                        .map_or(false, |body| body.to_lowercase().contains(&query))
                }
            })
            .map(|(idx, _url)| idx)
            .collect();

        if let Some(original_idx) = previously_selected_original_index {
            if let Some(new_filtered_pos) = self
                .filtered_url_indices
                .iter()
                .position(|&idx| idx == original_idx)
            {
                self.list_state.select(Some(new_filtered_pos));
            } else {
                self.select_first_or_last();
            }
        } else {
            self.select_first_or_last();
        }
    }

    fn select_first_or_last(&mut self) {
        if !self.filtered_url_indices.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }

    fn get_selected_original_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .and_then(|selected_filtered_idx| self.filtered_url_indices.get(selected_filtered_idx))
            .copied()
    }

    fn get_displayed_urls(&self) -> Vec<&String> {
        self.filtered_url_indices
            .iter()
            .map(|&idx| &self.visited_urls[idx])
            .collect()
    }

    fn get_selected_content(&self) -> Option<&String> {
        self.get_selected_original_index()
            .and_then(|original_idx| self.visited_urls.get(original_idx))
            .and_then(|url| self.body_texts.get(url))
    }

    fn get_selected_url_str(&self) -> Option<&str> {
        self.get_selected_original_index()
            .map(|original_idx| self.visited_urls[original_idx].as_str())
    }

    fn find_first_match_line(&self) -> Option<u16> {
        if self.active_search_query.is_empty() {
            return None;
        }
        let query_lower = self.active_search_query.to_lowercase();
        self.get_selected_content().and_then(|content| {
            content
                .lines()
                .position(|line| line.to_lowercase().contains(&query_lower))
                .map(|line_idx| line_idx as u16)
        })
    }

    fn reset_or_find_scroll(&mut self) {
        if !self.active_search_query.is_empty() {
            self.content_scroll = self.find_first_match_line().unwrap_or(0);
        } else {
            self.content_scroll = 0;
        }
    }

    // --- Methods for UI State Manipulation (mostly unchanged) ---
    fn select_next(&mut self) {
        let len = self.filtered_url_indices.len();
        if len == 0 {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % len,
            None => 0,
        };
        self.list_state.select(Some(i));
        self.reset_or_find_scroll();
    }

    fn select_previous(&mut self) {
        let len = self.filtered_url_indices.len();
        if len == 0 {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    len - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
        self.reset_or_find_scroll();
    }

    // Not used. Could be useful in future scenarios where we want to select the last item in the list.
    // fn select_last(&mut self) {
    //     let len = self.filtered_url_indices.len();
    //     if len > 0 {
    //         self.list_state.select(Some(len - 1));
    //     } else {
    //         self.list_state.select(None);
    //     }
    //     self.reset_or_find_scroll(); // Ensure scroll updates even when selecting last
    // }

    fn start_search(&mut self) {
        self.is_searching = true;
        self.search_input = self.active_search_query.clone();
    }

    fn finalize_search(&mut self) {
        self.is_searching = false;
        self.active_search_query = self.search_input.clone();
        self.update_filtered_list();
        self.reset_or_find_scroll();
    }

    fn cancel_search(&mut self) {
        self.is_searching = false;
        self.search_input.clear();
    }

    fn clear_search(&mut self) {
        self.is_searching = false;
        self.search_input.clear();
        self.active_search_query.clear();
        self.update_filtered_list();
        self.content_scroll = 0; // Reset scroll
    }

    // --- Scrolling Methods ---
    fn scroll_content_down(&mut self, lines: u16) {
        self.content_scroll = self.content_scroll.saturating_add(lines);
    }

    fn scroll_content_up(&mut self, lines: u16) {
        self.content_scroll = self.content_scroll.saturating_sub(lines);
    }
}

// --- Crawler Task (Unchanged) ---
type CrawlerMessage = (String, String);
async fn crawler_task(
    base_url: Url,
    tx: mpsc::Sender<CrawlerMessage>,
    url_queue: Arc<Mutex<VecDeque<String>>>,
    visited: Arc<Mutex<HashSet<String>>>,
) {
    let client = match Client::new(WEBDRIVER_URL).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to WebDriver at {}: {}", WEBDRIVER_URL, e);
            return;
        }
    };

    let base_domain = base_url.domain().unwrap_or("").to_string();

    while let Some(url) = { url_queue.lock().await.pop_front() } {
        if visited.lock().await.contains(&url) {
            continue;
        }

        if let Err(e) = client.goto(&url).await {
            eprintln!("Error navigating to {}: {}", url, e);
            visited.lock().await.insert(url);
            continue;
        }

        visited.lock().await.insert(url.clone());

        let body_text = match client.find(Locator::Css("body")).await {
            Ok(element) => match element.text().await {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("Error extracting text from <body> for {}: {}", url, e);
                    "<Body text extraction failed>".to_string()
                }
            },
            Err(_) => "<Body element not found>".to_string(),
        };

        if let Err(e) = tx.send((url.clone(), body_text)).await {
            eprintln!("Failed to send crawl result to main thread: {}", e);
            break;
        }

        match client.find_all(Locator::Css("a")).await {
            Ok(links) => {
                let mut queue = url_queue.lock().await;
                let visited_guard = visited.lock().await;

                for link in links {
                    if let Ok(Some(href)) = link.attr("href").await {
                        if let Ok(abs_url) = base_url.join(&href) {
                            if abs_url.domain().map_or(false, |d| d == base_domain) {
                                let abs_url_str = abs_url.to_string();
                                if !visited_guard.contains(&abs_url_str)
                                    && !queue.contains(&abs_url_str)
                                {
                                    queue.push_back(abs_url_str);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error finding links on {}: {}", url, e);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    if let Err(e) = client.close().await {
        eprintln!("Error closing WebDriver client: {}", e);
    }
}

// --- TUI Rendering ---

fn ui<B: tui::backend::Backend>(f: &mut Frame<B>, app_state: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ]
            .as_ref(),
        )
        .split(f.size());

    render_search_bar(f, app_state, chunks[0]);
    // Pass mutable state to render_main_content so it can update content_area
    render_main_content(f, app_state, chunks[1]);
    render_status_bar(f, chunks[2]);
}

fn render_search_bar<B: tui::backend::Backend>(f: &mut Frame<B>, app_state: &AppState, area: Rect) {
    let search_text = if app_state.is_searching {
        format!("Search: {}", app_state.search_input)
    } else if !app_state.active_search_query.is_empty() {
        format!(
            "Filtering by: \"{}\" (Press '/' to edit, Esc to clear)",
            app_state.active_search_query
        )
    } else {
        "Press '/' to search".to_string()
    };

    let search_widget = Paragraph::new(search_text)
        .style(if app_state.is_searching {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        })
        .block(Block::default().borders(Borders::ALL).title("Search"));
    f.render_widget(search_widget, area);
}

// This function now takes &mut AppState to update the content_area
fn render_main_content<B: tui::backend::Backend>(
    f: &mut Frame<B>,
    app_state: &mut AppState,
    area: Rect,
) {
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
        .split(area);

    // Update the content area in the state *before* rendering it
    // This ensures the mouse handler uses the correct area from the *current* frame
    app_state.content_area = content_chunks[1];

    render_url_list(f, app_state, content_chunks[0]);
    render_content_view(f, app_state, content_chunks[1]); // Pass immutable ref here is fine now
}

fn render_url_list<B: tui::backend::Backend>(
    f: &mut Frame<B>,
    app_state: &mut AppState,
    area: Rect,
) {
    let displayed_urls = app_state.get_displayed_urls();
    let items: Vec<ListItem> = displayed_urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            let display_url = if url.len() > area.width.saturating_sub(6) as usize {
                format!("{}...", &url[..area.width.saturating_sub(9) as usize])
            } else {
                url.to_string()
            };
            ListItem::new(Span::raw(format!("[{}] {}", i + 1, display_url)))
        })
        .collect();

    let list_widget = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Visited URLs ({})", displayed_urls.len())),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol(">> ");

    f.render_stateful_widget(list_widget, area, &mut app_state.list_state);
}

// Takes immutable AppState now, as content_area is set in the parent
fn render_content_view<B: tui::backend::Backend>(
    f: &mut Frame<B>,
    app_state: &AppState,
    area: Rect,
) {
    let selected_url_str = app_state
        .get_selected_url_str()
        .unwrap_or("<None Selected>");
    let content_title = format!(
        "Content (Scroll: {}): {}",
        app_state.content_scroll, selected_url_str
    );
    let block = Block::default().borders(Borders::ALL).title(content_title);

    let text = if let Some(content_raw) = app_state.get_selected_content() {
        if app_state.active_search_query.is_empty() {
            Text::from(content_raw.as_str())
        } else {
            // Highlighting logic (unchanged)
            let query = &app_state.active_search_query;
            let query_lower = query.to_lowercase();
            let mut spans_vec = Vec::new();
            for line in content_raw.lines() {
                let mut line_spans = Vec::new();
                let mut last_match_end = 0;
                let line_lower = line.to_lowercase();
                for (start_idx, _) in line_lower.match_indices(&query_lower) {
                    let end_idx = start_idx + query.len();
                    if start_idx > last_match_end {
                        line_spans.push(Span::raw(&line[last_match_end..start_idx]));
                    }
                    line_spans.push(Span::styled(
                        &line[start_idx..end_idx],
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ));
                    last_match_end = end_idx;
                }
                if last_match_end < line.len() {
                    line_spans.push(Span::raw(&line[last_match_end..]));
                }
                spans_vec.push(Spans::from(line_spans));
            }
            Text::from(spans_vec)
        }
    } else {
        Text::from("Select a URL to view its content.")
    };

    let content_widget = Paragraph::new(text)
        .block(block)
        .scroll((app_state.content_scroll, 0));

    f.render_widget(content_widget, area);
}

fn render_status_bar<B: tui::backend::Backend>(f: &mut Frame<B>, area: Rect) {
    // Updated help text reflects new keybindings
    let help_text = " Quit: Ctrl+Q | Back: Ctrl+C | Nav: ↑/↓/j/k | Search: / Enter Esc | Scroll: PgUp/PgDn/Mouse ";
    let status_widget =
        Paragraph::new(help_text).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(status_widget, area);
}

// --- Event Handling ---

enum AppControl {
    Continue,
    ExitCrawlerView, // Go back to URL prompt
    ExitApp,         // Quit entirely
}

// Handles key events specifically
fn handle_key_input(key: KeyEvent, app_state: &mut AppState) -> AppControl {
    if app_state.is_searching {
        match key.code {
            KeyCode::Enter => app_state.finalize_search(),
            KeyCode::Char(c) => app_state.search_input.push(c),
            KeyCode::Backspace => {
                app_state.search_input.pop();
            }
            KeyCode::Esc => app_state.cancel_search(),
            _ => {}
        }
    } else {
        match key.code {
            // Navigation
            KeyCode::Down | KeyCode::Char('j') => app_state.select_next(),
            KeyCode::Up | KeyCode::Char('k') => app_state.select_previous(),

            // Content Scrolling
            KeyCode::PageDown => app_state.scroll_content_down(SCROLL_LINES),
            KeyCode::PageUp => app_state.scroll_content_up(SCROLL_LINES),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app_state.scroll_content_down(SCROLL_LINES * 2) // Faster scroll with Ctrl+D/U
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app_state.scroll_content_up(SCROLL_LINES * 2)
            }

            // Search
            KeyCode::Char('/') => app_state.start_search(),
            KeyCode::Esc => {
                if !app_state.active_search_query.is_empty() {
                    app_state.clear_search();
                }
            }

            // Application Control (UPDATED)
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return AppControl::ExitApp; // Ctrl+Q quits the whole app
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return AppControl::ExitCrawlerView; // Ctrl+C exits the crawler view
            }

            _ => {}
        }
    }
    AppControl::Continue
}

// Handles mouse events specifically
fn handle_mouse_input(mouse_event: MouseEvent, app_state: &mut AppState) {
    // Check if the mouse coordinates are within the content panel's area
    if app_state
        .content_area
        .intersects(Rect::new(mouse_event.column, mouse_event.row, 1, 1))
    {
        match mouse_event.kind {
            MouseEventKind::ScrollDown => {
                app_state.scroll_content_down(SCROLL_LINES);
            }
            MouseEventKind::ScrollUp => {
                app_state.scroll_content_up(SCROLL_LINES);
            }
            // Could handle clicks here later if needed
            // MouseEventKind::Down(MouseButton::Left) => { ... }
            _ => {}
        }
    }
    // Ignore mouse events outside the content area for now
}

// --- Main Application Logic ---

// Setup terminal with mouse capture enabled
fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>, Box<dyn Error>> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?; // Enable mouse
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).map_err(|e| e.into())
}

// RAII guard ensures mouse capture is disabled on exit
struct RawModeGuard;
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Disable mouse capture BEFORE disabling raw mode
        execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture).ok();
        disable_raw_mode().ok();
    }
}

// Prompt for URL - updated keybindings
fn prompt_for_url<B: tui::backend::Backend>(
    terminal: &mut Terminal<B>,
) -> Result<Option<Url>, Box<dyn Error>> {
    let mut input_url = String::new();
    loop {
        terminal.draw(|f| {
            let size = f.size();
            // Updated prompt help text
            let prompt_text = format!(
                "Enter URL to crawl (Esc: back, Ctrl+Q: quit): {}",
                input_url
            );
            let paragraph = Paragraph::new(prompt_text)
                .block(Block::default().borders(Borders::ALL).title("Start URL"));
            f.render_widget(paragraph, size);
        })?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char(c) if !(key.modifiers.contains(KeyModifiers::CONTROL)) => {
                        input_url.push(c);
                    }
                    KeyCode::Backspace => {
                        input_url.pop();
                    }
                    KeyCode::Enter => match Url::parse(&input_url) {
                        Ok(url) if url.scheme() == "http" || url.scheme() == "https" => {
                            return Ok(Some(url));
                        }
                        _ => {
                            input_url.clear();
                        }
                    },
                    // Updated Quit Keys
                    KeyCode::Esc => return Ok(None), // Esc just exits prompt -> back to main loop check
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    } // Ctrl+Q exits app
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    } // Ctrl+C exits app from prompt
                    _ => {}
                }
            }
        }
    }
}

// Main app loop - updated event handling
async fn run_app<B: tui::backend::Backend>(
    terminal: &mut Terminal<B>,
    base_url: Url,
) -> Result<AppControl, Box<dyn Error>> {
    let mut app_state = AppState::new();
    let visited = Arc::new(Mutex::new(HashSet::new()));
    let url_queue = Arc::new(Mutex::new(VecDeque::from([base_url.to_string()])));
    let (tx, mut rx) = mpsc::channel::<CrawlerMessage>(CRAWLER_CHANNEL_BUFFER);

    let crawler_handle = tokio::spawn(crawler_task(
        base_url.clone(),
        tx,
        url_queue.clone(),
        visited.clone(),
    ));

    loop {
        // Draw UI - this now updates app_state.content_area
        terminal.draw(|f| ui(f, &mut app_state))?;

        // Handle incoming crawler messages
        while let Ok((url, body)) = rx.try_recv() {
            app_state.add_crawl_result(url, body);
        }

        // Handle Input Events (Key and Mouse)
        if event::poll(EVENT_POLL_TIMEOUT)? {
            match event::read()? {
                Event::Key(key_event) => {
                    // Handle key input using dedicated function
                    match handle_key_input(key_event, &mut app_state) {
                        AppControl::Continue => {} // Do nothing, continue loop
                        exit_command @ (AppControl::ExitCrawlerView | AppControl::ExitApp) => {
                            crawler_handle.abort();
                            return Ok(exit_command); // Return control signal
                        }
                    }
                }
                Event::Mouse(mouse_event) => {
                    // Handle mouse input using dedicated function
                    handle_mouse_input(mouse_event, &mut app_state);
                }
                Event::Resize(_, _) => {
                    // Re-rendering will happen automatically on next loop iteration
                    // Might want to clear screen or reset scroll here if needed
                }
                _ => {} // Ignore other event types
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut terminal = setup_terminal()?;
    let _raw_mode_guard = RawModeGuard; // RAII guard ensures cleanup

    loop {
        terminal.clear()?;

        let base_url = match prompt_for_url(&mut terminal)? {
            Some(url) => url,
            // If prompt_for_url returns None (Esc, Ctrl+Q, Ctrl+C), exit the app
            None => break,
        };

        match run_app(&mut terminal, base_url).await? {
            AppControl::ExitCrawlerView => continue, // Loop back to prompt_for_url
            AppControl::ExitApp => break,            // Exit the program entirely
            AppControl::Continue => unreachable!(),
        }
    }

    Ok(())
}
