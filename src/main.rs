use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use evdev::{InputEventKind, Key};
use futures::future::join_all;
use tokio::sync::{broadcast, Mutex};
use tokio::time::{self, Duration};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx1) = broadcast::channel(16);
    // let rx2 = tx.subscribe();
    let enable_key_sharing = Arc::new(Mutex::new(EnableKeySharing(true)));
    let device = pick_device();
    let kb_listener = tokio::spawn(keyboard_event_listener(
        device,
        tx,
        Arc::clone(&enable_key_sharing),
    ));
    let web_server = tokio::spawn(web_server(rx1, Arc::clone(&enable_key_sharing)));
    // let io_listener = tokio::spawn(io_listener(Arc::clone(&enable_key_sharing)));
    // let timeout_disabler = tokio::spawn(timeout_disabler(enable_key_sharing, rx2));

    let ress = join_all([kb_listener, web_server]).await;
    for res in ress.into_iter() {
        res??;
    }

    Ok(())
}

type KeyEvent = (Key, i32);
struct EnableKeySharing(bool);

async fn keyboard_event_listener(
    device: evdev::Device,
    tx: broadcast::Sender<KeyEvent>,
    enable_key_sharing: Arc<Mutex<EnableKeySharing>>,
) -> anyhow::Result<()> {
    let mut events = device.into_event_stream()?;
    loop {
        let ev = events.next_event().await?;
        if !enable_key_sharing.lock().await.0 {
            print!(".");
            use std::io::{self, Write};
            io::stdout().flush()?;
            continue;
        }
        let kind = ev.kind();
        let key = match kind {
            InputEventKind::Key(k) => k,
            _ => continue,
        };
        let value = ev.value();
        println!("Event: key={key:?}, value={value}");
        tx.send((key, value))?;
    }
}

async fn web_server(
    rx: broadcast::Receiver<KeyEvent>,
    enable_key_sharing: Arc<Mutex<EnableKeySharing>>,
) -> anyhow::Result<()> {
    let rx = Arc::new(Mutex::new(rx));
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .with_state((rx, enable_key_sharing));
    let addr = SocketAddr::from(([127, 0, 0, 1], 55238));
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

/// Enable and disable key sharing on command.
async fn io_listener(enable_key_sharing: Arc<Mutex<EnableKeySharing>>) -> anyhow::Result<()> {
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;
    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();
    while let Some(_line) = lines.next_line().await? {
        let mut enable_key_sharing = enable_key_sharing.lock().await;
        enable_key_sharing.0 = !enable_key_sharing.0;
        if enable_key_sharing.0 {
            println!("Key are being recorded.");
        } else {
            println!("Key sharing is paused.");
        }
    }
    Ok(())
}

/// Watches for keys and if a minute passes of inactivity, the keysharing is
/// disabled. Prevent password leaking.
async fn timeout_disabler(
    enable_key_sharing: Arc<Mutex<EnableKeySharing>>,
    mut rx: broadcast::Receiver<KeyEvent>,
) -> anyhow::Result<()> {
    loop {
        let timeout = time::sleep(Duration::from_secs(60));
        tokio::select! {
            Ok(_key_event) = rx.recv() => {
                // Key event received, will loop and create a new timeout
                print!("*");
                use std::io::Write;
                std::io::stdout().flush().unwrap();
                continue;
            },
            _ = timeout => {
                // Timeout reached - disable key sharing
                let mut lock = enable_key_sharing.lock().await;
                if !lock.0 {
                    continue;
                }
                lock.0 = false;
                println!("60 seconds since last key input. Key sharing is paused.");
            }
        }
    }
}

async fn ws_handler(
    State((rx, enable_key_sharing)): State<(
        Arc<Mutex<broadcast::Receiver<KeyEvent>>>,
        Arc<Mutex<EnableKeySharing>>,
    )>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, rx, enable_key_sharing))
}

async fn handle_socket(
    mut socket: WebSocket,
    rx: Arc<Mutex<broadcast::Receiver<KeyEvent>>>,
    enable_key_sharing: Arc<Mutex<EnableKeySharing>>,
) {
    while let Ok((key, value)) = rx.lock().await.recv().await {
        let paused = { !enable_key_sharing.lock().await.0 };
        let msg = format!("{{\"key\": \"{key:?}\", \"value\": {value}, \"paused\": {paused}}}");
        let _ = socket
            .send(axum::extract::ws::Message::Text(msg))
            .await
            .unwrap();
    }
}

pub fn pick_device() -> evdev::Device {
    use std::io::prelude::*;

    let mut args = std::env::args_os();
    args.next();
    if let Some(dev_file) = args.next() {
        evdev::Device::open(dev_file).unwrap()
    } else {
        let mut devices = evdev::enumerate().map(|t| t.1).collect::<Vec<_>>();
        // readdir returns them in reverse order from their eventN names for some reason
        devices.reverse();
        for (i, d) in devices.iter().enumerate() {
            println!("{}: {}", i, d.name().unwrap_or("Unnamed device"));
        }
        print!("Select the device [0-{}]: ", devices.len());
        let _ = std::io::stdout().flush();
        let mut chosen = String::new();
        std::io::stdin().read_line(&mut chosen).unwrap();
        let n = chosen.trim().parse::<usize>().unwrap();
        devices.into_iter().nth(n).unwrap()
    }
}
