use clap::Parser;
use serde_dispatch::serde_dispatch;
use std::error::Error;
use std::io::Write;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use time::{Duration, OffsetDateTime};

/// The name of the "updates" socket in the abstract namespace.
const SOCKET_NAME_UPDATES: &[u8] = b"waybar_timer_updates";
/// The name of the "commands" socket in the abstract namespace.
const SOCKET_NAME_COMMANDS: &[u8] = b"waybar_timer_commands";
/// The interval in which updates are pulled.
const INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

fn send_notification(summary: String) {
    let _ = notify_rust::Notification::new()
        .appname("Waybar Timer")
        .id(12345)
        .summary(&summary)
        .urgency(notify_rust::Urgency::Low)
        .show();
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
enum WorldError {
    NoTimerExisting,
    TimerAlreadyExisting,
}
impl std::fmt::Display for WorldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorldError::NoTimerExisting => write!(f, "no timer exists right now"),
            WorldError::TimerAlreadyExisting => write!(f, "there already exists a timer"),
        }
    }
}
impl Error for WorldError {}

#[serde_dispatch]
trait World {
    fn cancel(&mut self) -> Result<(), WorldError>;
    fn start(&mut self, command: Option<String>) -> Result<(), WorldError>;
    fn increase(&mut self, seconds: i64) -> Result<(), WorldError>;
    fn togglepause(&mut self) -> Result<(), WorldError>;
    fn skip(&mut self) -> Result<(), WorldError>;
}

#[derive(Debug)]
struct Timer {
    cycles: i32,
    kind: TimerKind,
}

#[derive(Debug)]
enum TimerKind {
    Idle,
    Running {
        expiry: OffsetDateTime,
        command: Option<String>,
    },
    Paused {
        time_left: Duration,
        command: Option<String>,
    },
}

impl Timer {
    /// updates timer, potentially executes action, and returns formatted string for waybar
    fn update(&mut self) -> String {
        let now = OffsetDateTime::now_local().unwrap();

        // check if timer expired
        if let TimerKind::Running { expiry, command } = &self.kind {
            let time_left = *expiry - now;
            if time_left <= Duration::ZERO {
                // timer has expired, send notification and set timer to idle
                if let Some(command) = command {
                    let _ = std::process::Command::new("bash")
                        .arg("-c")
                        .arg(command)
                        .output();
                }
                *self = Timer {
                    kind: TimerKind::Idle,
                    cycles: self.cycles + 1,
                };
            }
        }

        let focus_break = if self.cycles % 2 == 0 {
            "focus"
        } else {
            "break"
        };

        // print new output to stdout (for waybar)
        let (text, alt, tooltip, css_class) = match self.kind {
            TimerKind::Idle { .. } => (0, "standby", "No timer set".into(), "idle"),
            TimerKind::Running { expiry, .. } => {
                let time_left = expiry - now;
                let minutes_left = time_left.whole_minutes() + 1;
                let tooltip = Self::tooltip(&expiry);
                (minutes_left, "running", tooltip, focus_break)
            }
            TimerKind::Paused { time_left, .. } => {
                let minutes_left = time_left.whole_minutes() + 1;
                let tooltip = "Timer paused".into();
                (minutes_left, "paused", tooltip, focus_break)
            }
        };

        format!("{{\"text\": \"{text}\", \"alt\": \"{alt}-{focus_break}\", \"tooltip\": \"{tooltip}\", \"class\": \"{css_class}\"}}")
    }

    fn tooltip(expiry: &OffsetDateTime) -> String {
        let format_desc = time::macros::format_description!("[hour]:[minute]");
        let expiry_str = expiry.format(&format_desc).unwrap();
        format!("Timer expires at {expiry_str}")
    }
}

impl World for Timer {
    fn cancel(&mut self) -> Result<(), WorldError> {
        match self.kind {
            TimerKind::Idle => {
                self.cycles = 0;
            }
            _ => send_notification("Timer canceled".into()),
        };
        self.kind = TimerKind::Idle;
        Ok(())
    }

    fn start(&mut self, command: Option<String>) -> Result<(), WorldError> {
        let minutes = match self.cycles % 8 {
            1 | 3 | 5 => 5,
            _ => 25,
        };
        match self.kind {
            TimerKind::Idle => {
                let expiry = OffsetDateTime::now_local().unwrap()
                    + Duration::minutes(minutes.into())
                    - Duration::MILLISECOND;
                send_notification(Self::tooltip(&expiry));
                self.kind = TimerKind::Running { expiry, command };
                Ok(())
            }
            TimerKind::Paused { .. } | TimerKind::Running { .. } => self.togglepause(),
        }
    }

    fn increase(&mut self, seconds: i64) -> Result<(), WorldError> {
        match self.kind {
            TimerKind::Running { ref mut expiry, .. } => {
                *expiry += Duration::seconds(seconds);
                send_notification(Self::tooltip(&expiry));
                Ok(())
            }
            TimerKind::Paused {
                ref mut time_left,
                command: _,
            } => {
                *time_left += Duration::seconds(seconds);
                Ok(())
            }
            TimerKind::Idle => Err(WorldError::NoTimerExisting),
        }
    }

    fn skip(&mut self) -> Result<(), WorldError> {
        match self.kind {
            TimerKind::Idle => Err(WorldError::NoTimerExisting),
            TimerKind::Running { ref mut expiry, .. } => {
                *expiry = OffsetDateTime::now_local().unwrap();
                Ok(())
            }
            TimerKind::Paused {
                ref mut time_left, ..
            } => {
                *time_left = Duration::ZERO;
                self.togglepause()
            }
        }
    }

    fn togglepause(&mut self) -> Result<(), WorldError> {
        match self.kind {
            TimerKind::Running {
                expiry,
                ref mut command,
            } => {
                let time_left = expiry - OffsetDateTime::now_local().unwrap();
                send_notification("Timer paused".into());
                self.kind = TimerKind::Paused {
                    time_left,
                    command: command.take(),
                };
                Ok(())
            }
            TimerKind::Paused {
                time_left,
                ref mut command,
            } => {
                let expiry = OffsetDateTime::now_local().unwrap() + time_left;
                send_notification(Self::tooltip(&expiry));
                self.kind = TimerKind::Running {
                    expiry,
                    command: command.take(),
                };
                Ok(())
            }
            TimerKind::Idle => Err(WorldError::NoTimerExisting),
        }
    }
}

/// Waybar Timer (see https://github.com/jbirnick/waybar-timer/)
#[derive(Parser)]
enum Args {
    /// Serve a timer API (should be called once at compositor startup)
    Serve,
    /// Keep reading the latest status of the timer (should be called by waybar)
    Hook,
    /// Start a new timer
    New {
        command: Option<String>,
    },
    /// Increase the current timer
    Increase {
        seconds: u32,
    },
    /// Decrease the current timer
    Decrease {
        seconds: u32,
    },
    /// Pause or resume the current timer
    Togglepause,
    Skip,
    /// Cancel the current timer
    Cancel,
}

struct ServerState {
    timer: Timer,
    subs: Vec<UnixStream>,
}

impl ServerState {
    fn update(&mut self) {
        // update timer and get waybar string
        let message = self.timer.update();

        // broadcast it to subscribers
        let mut i: usize = 0;
        loop {
            if i == self.subs.len() {
                break;
            }
            match writeln!(self.subs[i], "{}", message) {
                Ok(()) => {
                    let _ = self.subs[i].flush();
                    i += 1;
                }
                Err(err) => {
                    println!("couldn't write to subscriber stream: {}", err);
                    println!("will drop the subscriber");
                    self.subs.swap_remove(i);
                }
            }
        }
    }
}

fn run_serve() {
    let state = Arc::new(Mutex::new(ServerState {
        timer: Timer {
            cycles: 0,
            kind: TimerKind::Idle,
        },
        subs: Vec::new(),
    }));

    // spawn a thread which is responsible for calling update in a regular interval
    let state_thread_interval = state.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(INTERVAL);
        let mut state = state_thread_interval.lock().unwrap();
        state.update();
    });

    // spawn a thread which is responsible for accepting new subscribers
    let state_thread_subaccept = state.clone();
    std::thread::spawn(move || {
        let listener =
            UnixListener::bind_addr(&SocketAddr::from_abstract_name(SOCKET_NAME_UPDATES).unwrap())
                .expect("couldn't connect to the \"update\" socket");
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    // put to list of subscribers and trigger update so that
                    // the new subscriber gets the current state
                    let mut state = state_thread_subaccept.lock().unwrap();
                    stream.shutdown(std::net::Shutdown::Read).unwrap();
                    state.subs.push(stream);
                    state.update();
                }
                Err(err) => {
                    panic!("{err}")
                }
            }
        }
    });

    // the main thread handles requests from the CLI
    let listener =
        UnixListener::bind_addr(&SocketAddr::from_abstract_name(SOCKET_NAME_COMMANDS).unwrap())
            .expect("couldn't connect to the \"commands\" socket");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // handles a single remote procedure call
                let mut state = state.lock().unwrap();
                state.timer.handle_with(&stream, &stream).unwrap();
                stream.shutdown(std::net::Shutdown::Both).unwrap();
                state.update();
            }
            Err(err) => {
                panic!("{err}")
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let socket_addr_updates = SocketAddr::from_abstract_name(SOCKET_NAME_UPDATES).unwrap();
    let socket_addr_commands = SocketAddr::from_abstract_name(SOCKET_NAME_COMMANDS).unwrap();
    let args = Args::parse();
    match args {
        Args::Serve => {
            run_serve();
            Ok(())
        }
        Args::Hook => {
            let mut stream = UnixStream::connect_addr(&socket_addr_updates)?;
            stream.shutdown(std::net::Shutdown::Write)?;
            let mut stdout = std::io::stdout();
            std::io::copy(&mut stream, &mut stdout)?;
            Ok(())
        }
        Args::New { command } => {
            let stream = UnixStream::connect_addr(&socket_addr_commands)?;
            WorldRPCClient::call_with(&stream, &stream).start(&command)??;
            stream.shutdown(std::net::Shutdown::Both)?;
            Ok(())
        }
        Args::Increase { seconds } => {
            let stream = UnixStream::connect_addr(&socket_addr_commands)?;
            WorldRPCClient::call_with(&stream, &stream).increase(&seconds.into())??;
            stream.shutdown(std::net::Shutdown::Both)?;
            Ok(())
        }
        Args::Decrease { seconds } => {
            let seconds: i64 = seconds.into();
            let stream = UnixStream::connect_addr(&socket_addr_commands)?;
            WorldRPCClient::call_with(&stream, &stream).increase(&-seconds)??;
            stream.shutdown(std::net::Shutdown::Both)?;
            Ok(())
        }
        Args::Togglepause => {
            let stream = UnixStream::connect_addr(&socket_addr_commands)?;
            WorldRPCClient::call_with(&stream, &stream).togglepause()??;
            stream.shutdown(std::net::Shutdown::Both)?;
            Ok(())
        }
        Args::Skip => {
            let stream = UnixStream::connect_addr(&socket_addr_commands)?;
            WorldRPCClient::call_with(&stream, &stream).skip()??;
            stream.shutdown(std::net::Shutdown::Both)?;
            Ok(())
        }
        Args::Cancel => {
            let stream = UnixStream::connect_addr(&socket_addr_commands)?;
            WorldRPCClient::call_with(&stream, &stream).cancel()??;
            stream.shutdown(std::net::Shutdown::Both)?;
            Ok(())
        }
    }
}
