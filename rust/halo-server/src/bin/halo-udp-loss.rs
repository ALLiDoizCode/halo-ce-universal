//! A lossy link for the real game: a UDP relay that drops a share of the
//! datagrams, each way, between a client and a gateway. The game has no loss of
//! its own to switch on in the large-scale mode, so the check that asks how it
//! plays on a link that loses 2% points the game at this instead of the gateway:
//!
//!   halo-udp-loss --listen 127.0.0.1:7790 --to 127.0.0.1:7777 --loss 0.02
//!   HALO_LARGE_GATEWAY=127.0.0.1:7790 ./halo ...
//!
//! Each client address gets a socket of its own towards the gateway, so that the
//! gateway sees one address per player, as it does without the relay. Counts are
//! printed to stderr every 10 seconds.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use halo_sim::Rng;

#[derive(Default)]
struct Counts {
    up_sent: AtomicU64,
    up_dropped: AtomicU64,
    down_sent: AtomicU64,
    down_dropped: AtomicU64,
}

fn main() {
    let args: HashMap<String, String> = std::env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .chunks(2)
        .filter_map(|c| Some((c.first()?.trim_start_matches("--").to_string(), c.get(1)?.clone())))
        .collect();
    let get = |k: &str, d: &str| args.get(k).cloned().unwrap_or_else(|| d.to_string());
    let listen: SocketAddr = get("listen", "127.0.0.1:7790").parse().unwrap_or_else(|_| fail("--listen is host:port"));
    let to: SocketAddr = get("to", "127.0.0.1:7777").parse().unwrap_or_else(|_| fail("--to is host:port"));
    let loss: f32 = get("loss", "0.02").parse().unwrap_or_else(|_| fail("--loss is a number from 0 to 1"));

    let socket = Arc::new(UdpSocket::bind(listen).unwrap_or_else(|e| fail(&format!("binding {listen}: {e}"))));
    let counts = Arc::new(Counts::default());
    let rng = Arc::new(Mutex::new(Rng::seeded(0x1055)));
    let upstreams: Arc<Mutex<HashMap<SocketAddr, Arc<UdpSocket>>>> = Arc::default();
    {
        let counts = counts.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(10));
            eprintln!(
                "[loss] to the gateway: {} sent, {} dropped; to the clients: {} sent, {} dropped",
                counts.up_sent.load(Relaxed),
                counts.up_dropped.load(Relaxed),
                counts.down_sent.load(Relaxed),
                counts.down_dropped.load(Relaxed)
            );
        });
    }
    eprintln!("[loss] relaying {listen} to {to}, losing {:.1}% each way", loss * 100.0);
    let mut buf = vec![0u8; 2048];
    loop {
        let Ok((len, client)) = socket.recv_from(&mut buf) else { continue };
        let up = upstreams
            .lock()
            .unwrap()
            .entry(client)
            .or_insert_with(|| {
                let up = Arc::new(UdpSocket::bind("127.0.0.1:0").expect("a socket towards the gateway"));
                up.connect(to).expect("connect to the gateway");
                let (up_reader, down, counts, rng) = (up.clone(), socket.clone(), counts.clone(), rng.clone());
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; 2048];
                    while let Ok(len) = up_reader.recv(&mut buf) {
                        if rng.lock().unwrap().next_f32() < loss {
                            counts.down_dropped.fetch_add(1, Relaxed);
                        } else {
                            counts.down_sent.fetch_add(1, Relaxed);
                            let _ = down.send_to(&buf[..len], client);
                        }
                    }
                });
                up
            })
            .clone();
        if rng.lock().unwrap().next_f32() < loss {
            counts.up_dropped.fetch_add(1, Relaxed);
        } else {
            counts.up_sent.fetch_add(1, Relaxed);
            let _ = up.send(&buf[..len]);
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("halo-udp-loss: {message}");
    std::process::exit(2);
}
