use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use evdev::{InputEventKind, Key};
use futures::future::join_all;
use tokio::io::AsyncBufReadExt;
use tokio::sync::{broadcast, Mutex};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (tx, mut rx1) = broadcast::channel(16);
    let tx = Arc::new(Mutex::new(tx));
    let kb_listener = {
        let tx = Arc::clone(&tx);
        let device = pick_device();
        tokio::spawn(keyboard_event_listener(device, tx))
    };
    let web_server = tokio::spawn(web_server(tx));

    let ress = join_all([kb_listener, web_server]).await;
    for res in ress.into_iter() {
        res??;
    }

    Ok(())
}

async fn ws_handler(State(tx): State<SharedSender>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, tx))
}

async fn handle_socket(mut socket: WebSocket, tx: SharedSender) {
    let mut rx = tx.lock().await.subscribe();
    while let Ok((key, value)) = rx.recv().await {
        let msg = format!("{{\"key\": \"{key:?}\", \"value\": {value}}}");
        socket
            .send(axum::extract::ws::Message::Text(msg))
            .await
            .unwrap();
    }
}

type KeyEvent = (Key, i32);
type SharedSender = Arc<Mutex<broadcast::Sender<KeyEvent>>>;

async fn keyboard_event_listener(device: evdev::Device, tx: SharedSender) -> anyhow::Result<()> {
    let mut events = device.into_event_stream()?;
    loop {
        let ev = events.next_event().await?;
        let kind = ev.kind();
        let key = match kind {
            InputEventKind::Key(k) => k,
            _ => continue,
        };
        let value = ev.value();
        println!("Event: key={key:?}, value={value}");
        tx.lock().await.send((key, value))?;
    }
}

async fn web_server(tx: SharedSender) -> anyhow::Result<()> {
    let app = Router::new().route("/ws", get(ws_handler)).with_state(tx);
    let addr = SocketAddr::from(([127, 0, 0, 1], 55238));
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?;
    Ok(())
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
