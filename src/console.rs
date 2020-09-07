use std::error::Error;
use std::io;
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use crossbeam_channel::Receiver;
use termion::event::Key;
use termion::input::MouseTerminal;
use termion::input::TermRead;
use termion::raw::IntoRawMode;
use termion::screen::AlternateScreen;
use tui::backend::TermionBackend;
use tui::layout::{Constraint, Direction, Layout};
use tui::style::{Color, Style};
use tui::widgets::{Block, Borders, Paragraph, Text};
use tui::Terminal;
use tui_logger::*;

/* TODO
 *
 * Seperate Stats between node and network
 * Ability to scroll through the log output
 * Ability to change the logging level
 * Add tabs support (Network, Ledger, Wallet)
 *
 *
*/

pub enum Event<I> {
    Input(I),
    Tick,
}

/// A small event handler that wrap termion input and tick events. Each event
/// type is handled in its own thread and returned to a common `Receiver`
pub struct Events {
    rx: mpsc::Receiver<Event<Key>>,
    ignore_exit_key: Arc<AtomicBool>,
}

impl Default for Events {
    fn default() -> Self {
        Self::with_config(Config::default())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub exit_key: Key,
    pub tick_rate: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            exit_key: Key::Char('q'),
            tick_rate: Duration::from_millis(250),
        }
    }
}

impl Events {
    pub fn with_config(config: Config) -> Events {
        let (tx, rx) = mpsc::channel();
        let ignore_exit_key = Arc::new(AtomicBool::new(false));
        {
            let tx = tx.clone();
            let ignore_exit_key = ignore_exit_key.clone();
            thread::spawn(move || {
                let stdin = io::stdin();
                for evt in stdin.keys() {
                    if let Ok(key) = evt {
                        if let Err(err) = tx.send(Event::Input(key)) {
                            eprintln!("{}", err);
                            return;
                        }
                        if !ignore_exit_key.load(Ordering::Relaxed) && key == config.exit_key {
                            return;
                        }
                    }
                }
            });
        }
        thread::spawn(move || loop {
            if tx.send(Event::Tick).is_err() {
                break;
            }
            thread::sleep(config.tick_rate);
        });
        Events {
            rx,
            ignore_exit_key,
        }
    }

    pub fn next(&self) -> Result<Event<Key>, mpsc::RecvError> {
        self.rx.recv()
    }

    pub fn disable_exit_key(&mut self) {
        self.ignore_exit_key.store(true, Ordering::Relaxed);
    }

    pub fn enable_exit_key(&mut self) {
        self.ignore_exit_key.store(false, Ordering::Relaxed);
    }
}

pub struct AppState {
    pub node_type: String,
    pub node_id: String,
    pub node_addr: String,
    pub connections: String,
    pub peers: String,
    pub pieces: String,
    pub blocks: String,
}

pub struct App {
    state: AppState,
    receiver: Receiver<AppState>,
}

impl App {
    fn new(receiver: Receiver<AppState>) -> App {
        App {
            state: AppState {
                node_type: String::from(""),
                node_id: String::from(""),
                node_addr: String::from(""),
                connections: String::from(""),
                peers: String::from(""),
                pieces: String::from(""),
                blocks: String::from(""),
            },
            receiver,
        }
    }

    // maybe work with std channel
    fn update(&mut self) {
        if let Ok(state) = self.receiver.try_recv() {
            self.state = state
        }
    }
}

pub fn run(recever: Receiver<AppState>) -> Result<(), Box<dyn Error>> {
    log::info!("Running app");

    // setup the terminal
    let stdout = io::stdout().into_raw_mode()?;

    // clear the background
    let stdout = MouseTerminal::from(stdout);
    let stdout = AlternateScreen::from(stdout);
    let backend = TermionBackend::new(stdout);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.clear().unwrap();
    terminal.hide_cursor().unwrap();

    // handle events and app state
    let events = Events::default();
    let mut app = App::new(recever);

    // setup the ui
    loop {
        terminal.draw(|mut f| {
            let vertical_chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(0)
                .constraints([Constraint::Percentage(10), Constraint::Percentage(90)].as_ref())
                .split(f.size());

            let title_text = [Text::raw(" Subspace Network Console ")];

            let title = Paragraph::new(title_text.iter())
                .block(Block::default().title("").borders(Borders::ALL))
                .style(Style::default());

            f.render_widget(title, vertical_chunks[0]);

            let horizontal_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .margin(0)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
                .split(vertical_chunks[1]);

            let text = [
                Text::raw("\n"),
                Text::raw(" Node Type      "),
                Text::raw(String::from(&app.state.node_type)),
                Text::raw("\n"),
                Text::raw(" Node ID        "),
                Text::raw(String::from(&app.state.node_id)),
                Text::raw("\n"),
                Text::raw(" Address        "),
                Text::raw(String::from(&app.state.node_addr)),
                Text::raw("\n"),
                Text::raw(" Connections    "),
                Text::raw(String::from(&app.state.connections)),
                Text::raw("\n"),
                Text::raw(" Peers          "),
                Text::raw(String::from(&app.state.peers)),
                Text::raw("\n"),
                Text::raw(" Pieces         "),
                Text::raw(String::from(&app.state.pieces)),
                Text::raw("\n"),
                Text::raw(" Blocks         "),
                Text::raw(String::from(&app.state.blocks)),
                Text::raw("\n"),
            ];

            let paragraph = Paragraph::new(text.iter())
                .block(Block::default().title(" Stats ").borders(Borders::ALL))
                .style(Style::default());

            f.render_widget(paragraph, horizontal_chunks[0]);

            let tui_w: TuiLoggerWidget = TuiLoggerWidget::default()
                .block(
                    Block::default()
                        .title(" Log ")
                        .title_style(Style::default())
                        .border_style(Style::default())
                        .borders(Borders::ALL),
                )
                .style_error(Style::default().fg(Color::Red))
                .style_debug(Style::default().fg(Color::Green))
                .style_warn(Style::default().fg(Color::Yellow))
                .style_trace(Style::default().fg(Color::Magenta))
                .style_info(Style::default().fg(Color::Cyan));

            f.render_widget(tui_w, horizontal_chunks[1]);
        })?;

        // allow for exit
        match events.next()? {
            Event::Input(input) => {
                if input == Key::Char('q') {
                    break;
                }
            }
            Event::Tick => {
                app.update();
            }
        }
    }
    Ok(())
}
