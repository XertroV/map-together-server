
mod msgs;
mod managers;
mod map_actions;
mod mt_room;
mod op_auth;
mod mt_codec;
mod player_loop;
mod consts;
mod common;

pub use consts::*;
pub use common::*;

use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use lazy_static::lazy_static;
use rand::prelude::SliceRandom;
use rand::Rng;
use tokio::fs::{read, read_dir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{oneshot, OnceCell, RwLock};
use tokio::time;

use crate::map_actions::*;
use crate::msgs::{PlayerCamCursor, INIT_MSG_ROOM_CREATE, INIT_MSG_ROOM_JOIN};
use crate::mt_codec::MTEncode;
use crate::managers::*;

lazy_static! {
    static ref COUNT_BOTS: Arc<RwLock<u32>> = Arc::new(RwLock::new(0));
    static ref COUNT_SENT: Arc<RwLock<u32>> = Arc::new(RwLock::new(0));
    static ref COUNT_RECV: Arc<RwLock<u32>> = Arc::new(RwLock::new(0));
    static ref ROOM_IDS: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(vec![]));
}

struct ClientBot {
    stream: TcpStream,
    shutdown: Option<oneshot::Receiver<()>>,
    shutdown_flag: Arc<OnceCell<bool>>,
    sent_count: Arc<RwLock<u32>>,
    recv_count: Arc<RwLock<u32>>,
}

impl ClientBot {
    async fn new_run(addr: SocketAddr) -> Result<(Self, oneshot::Sender<()>), Box<dyn Error>> {
        let stream = tokio::net::TcpStream::connect(addr).await?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        Ok((
            ClientBot {
                stream,
                shutdown: Some(shutdown_rx),
                shutdown_flag: OnceCell::new().into(),
                sent_count: Arc::new(RwLock::new(0)),
                recv_count: Arc::new(RwLock::new(0)),
            },
            shutdown_tx,
        ))
    }

    async fn init_connection(&mut self, join: bool, token: Option<String>) -> Result<(), Box<dyn Error>> {
        write_lp_string(&mut self.stream, &token.unwrap_or("test_token".to_string())).await.unwrap();

        self.stream.write_all(&CURR_VERSION_BYTES).await.unwrap();

        let mut buf = [0u8; 3];
        self.stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"OK_");
        let room_id;
        if join {
            self.stream.write_u8(INIT_MSG_ROOM_JOIN).await.unwrap();
            write_lp_string(&mut self.stream, ROOM_IDS.read().await.last().unwrap())
                .await
                .unwrap();
            write_lp_string(&mut self.stream, "test_pw").await.unwrap();

            room_id = read_lp_string(&mut self.stream).await.unwrap();
        } else {
            println!("Creating room");
            self.stream.write_u8(INIT_MSG_ROOM_CREATE).await.unwrap();
            write_lp_string(&mut self.stream, "test_pw").await.unwrap();
            println!("Sent room create and pw");

            let deets = [0u8; 14];
            self.stream.write_all(&deets).await.unwrap();
            println!("Sent room creation deets");

            // let mut buf = [0u8; 3];
            // self.stream.read_exact(&mut buf).await.unwrap();
            // assert_eq!(&buf, b"OK_");

            room_id = read_lp_string(&mut self.stream).await.unwrap();

            // 2 * 4 + 6 * 1 bytes = 14
            // self.stream.read_exact(&mut [0u8; 14]).await.unwrap();
        }
        assert_eq!(room_id.len(), 6);
        let init_ty = match join {
            true => "Join",
            false => "Create",
        };
        println!("[Bot {}] Room ID: {}", init_ty, room_id);
        if !join {
            println!("Adding to ROOM_IDS: Room ID: {}", &room_id);
            ROOM_IDS.write().await.push(room_id.clone());
            println!("Added to ROOM_IDS: Room ID: {}", room_id);
        }
        let mut buf = [0u8; 14];
        self.stream.read_exact(&mut buf).await?;
        assert_eq!(buf, [0u8; 14], "we wrote zeros for the room creation deets");
        Ok(())
    }

    async fn run(mut self, join: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let mut mb_dir = read_dir("./mb-samples").await?;
        // load all files in the mb-samples directory
        let mut mbs: Vec<Vec<u8>> = vec![];
        while let Some(entry) = mb_dir.next_entry().await? {
            // println!("Found entry: {:?}", entry.path());
            if entry.file_type().await?.is_file()
                && entry.path().extension().unwrap_or_default() == "mb_spec"
            {
                let mut buf = read(entry.path()).await?;
                // let len: u32 = u32::from_le_bytes(buf[1..5].try_into().unwrap());
                let _ = buf.split_off(buf.len() - META_BYTES);
                mbs.push(buf);
            } else {
                println!("Skipping non-mb entry: {:?}", entry.path());
            }
        }
        assert_ne!(0, mbs.len(), "should have some MBs");
        println!("Loaded {} MBs", mbs.len());

        let mut shutdown = self.shutdown.take().unwrap();
        self.init_connection(join, None).await?;

        let (mut r, mut w) = self.stream.into_split();
        let shutdown_flag2 = self.shutdown_flag.clone();
        let my_sent = self.sent_count.clone();
        let my_recv = self.recv_count.clone();
        let read_handle = tokio::spawn(async move {
            loop {
                while !r.readable().await.is_ok() {
                    time::sleep(Duration::from_millis(1)).await;
                }
                let x = r.read_u8().await?;
                assert!(
                    x == 1 || x == 14 || x == 7 || x == 8,
                    "only place / cursor / join / leave msgs"
                );
                let len = r.read_u32_le().await?;
                assert!(len > 0, "len > 0");
                let mut buf = vec![0u8; len as usize];
                r.read_exact(&mut buf).await?;
                time::sleep(Duration::from_millis(1)).await;
                let _id = read_lp_string_owh(&mut r).await?;
                let _ts = r.read_u64_le().await?;

                *my_recv.write().await += 1;
                *COUNT_RECV.write().await += 1;
                // println!("Bot Recv: {} {} - {} {:?}", x, len, _id, _ts);

                if shutdown_flag2.initialized() {
                    break;
                }
            }
            Ok::<_, StreamErr>(r)
        });

        loop {
            tokio::select! {
                // rand::thread_rng().gen_range(100)
                _ = time::sleep(Duration::from_millis(100)) => {
                    let cam: PlayerCamCursor = rand::thread_rng().gen();
                    let buf = cam.encode();
                    w.write_u8(MAPPING_MSG_PLAYER_CAMCURSOR).await?;
                    time::sleep(Duration::from_millis(1)).await;
                    w.write_u32_le(buf.len() as u32).await?;
                    time::sleep(Duration::from_millis(1)).await;
                    w.write_all(&buf).await?;
                    *my_sent.write().await += 1;
                    tokio::spawn(async move {
                        *COUNT_SENT.write().await += 1;
                    });
                    // 10% chance to send a block
                    if rand::thread_rng().gen::<usize>() % 10 == 0 {
                        w.write_u8(MAPPING_MSG_PLACE).await?;
                        let mb = mbs.choose(&mut rand::thread_rng()).unwrap();
                        w.write_u32_le(mb.len() as u32).await?;
                        time::sleep(Duration::from_millis(1)).await;
                        w.write_all(&mb).await?;
                        *my_sent.write().await += 1;
                    }
                }
            }
            if shutdown.try_recv().is_ok() {
                self.shutdown_flag.set(true).unwrap();
                w.shutdown().await?;
                break;
            }
        }

        let r = read_handle.await??;

        self.stream = w.reunite(r)?;

        Ok(self)
    }
}

async fn run_client_bot() {
    // start server
    // let mut rng = rand::thread_rng();

    let (listener, init_manager, room_manager) = get_server_things(Default::default()).await;
    let listener_addr = listener.local_addr().unwrap();
    let _init_mgr = init_manager.clone();
    let room_mgr = room_manager.clone();

    tokio::spawn(async move {
        run_server(listener, init_manager, room_manager).await;
    });

    let start = SystemTime::now();
    let since_start = || start.elapsed().unwrap().as_millis();

    // plan: start 1 bot per second and monitor counts
    // go up to 200 bots, then wait for 10 seconds and print stats

    let bots: Arc<RwLock<Vec<ClientBot>>> = Default::default();
    let shutdowns = Arc::new(RwLock::new(vec![]));
    for i in 0..200 {
        let _shutdowns = shutdowns.clone();
        let _bots = bots.clone();
        println!(
            "[{} ms] Creating Bot {}",
            since_start(),
            COUNT_BOTS.read().await
        );
        tokio::spawn(async move {
            let (bot, shutdown) = ClientBot::new_run(listener_addr).await.unwrap();
            _shutdowns.write().await.push(shutdown);
            let bot = bot.run(i > 0).await.unwrap();
            _bots.write().await.push(bot);
        });
        *COUNT_BOTS.write().await += 1;
        println!(
            "[{} ms] Message Stats: Total Sent: {}, Total Recv: {}",
            since_start(),
            *COUNT_SENT.read().await,
            *COUNT_RECV.read().await
        );
        time::sleep(Duration::from_millis(
            rand::thread_rng().gen_range(900..1100),
        ))
        .await;
        if i == 0 {
            while ROOM_IDS.read().await.last().is_none() {
                time::sleep(Duration::from_millis(100)).await;
                println!("Waiting for first room to be created");
            }
            println!("First node has joined a room");
        }
    }

    time::sleep(Duration::from_secs(10)).await;

    for shutdown in shutdowns.write().await.drain(0..) {
        shutdown.send(()).unwrap();
    }

    time::sleep(Duration::from_secs(1)).await;

    let sent = COUNT_SENT.read().await;
    let recv = ROOM_IDS.read().await.len() as u32;
    println!("Sent: {}, Recv: {}", sent, recv);
    for (id, room) in room_mgr.read().await.rooms.read().await.iter() {
        println!(
            "Room: {} | Actions Stored: {}",
            room.id_str,
            room.actions.read().await.len()
        );
    }
    for (i, bot) in bots.read().await.iter().enumerate() {
        println!(
            "Bot {} Sent: {}, Bot Recv: {}",
            i,
            bot.sent_count.read().await,
            bot.recv_count.read().await
        );
    }
}


#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    run_client_bot().await;
}


mod tests {

    #[tokio::test(flavor = "multi_thread", worker_threads = 10)]
    async fn test_client_bots() {
        super::run_client_bot().await;
    }
}
