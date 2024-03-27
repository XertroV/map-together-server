use futures::Future;
use log::LevelFilter;
use managers::*;
use op_auth::*;
use rand::Rng;
use simple_logger::SimpleLogger;
use std::sync::{Arc, OnceLock};
use tokio::{net::TcpListener, sync::{RwLock}};
pub use consts::*;
pub use common::*;

// #[cfg(test)]
// #[macro_use]
// extern quickcheck;

mod common;
mod consts;
mod msgs;
mod managers;
mod map_actions;
mod mt_room;
mod op_auth;
mod mt_codec;
mod player_loop;


#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    let args: Vec<_> = std::env::args().collect();

    let dump_macroblocks = args.contains(&"--dump-macroblocks".into());
    DUMP_MBS.get_or_init(|| { dump_macroblocks });

    let logger = SimpleLogger::new()
        .with_level(LevelFilter::Trace)
        .with_colors(true);
    log::set_max_level(LevelFilter::Trace);
    log::set_boxed_logger(Box::new(logger)).unwrap();

    // Initialize your application configurations
    let nb_configs = init_op_config().await;
    log::info!("Configurations initialized: {}", nb_configs);

    let (listener, init_manager, room_manager) = get_server_things(ServerOpts {
        dump_macroblocks, ..Default::default()
    }).await;

    run_server(listener, init_manager, room_manager).await;
}




#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::msgs::INIT_MSG_ROOM_CREATE;

    use super::*;

    #[tokio::test]
    async fn test_init_op_config() {
        let nb_configs = init_op_config().await;
        assert_eq!(nb_configs, 1);
    }

    #[tokio::test]
    async fn check_simple_connection() {
        let (listener, init_manager, room_manager) = get_server_things(Default::default()).await;
        let listener_addr: std::net::SocketAddr = listener.local_addr().unwrap();
        let _init_mgr = init_manager.clone();
        let room_mgr = room_manager.clone();

        tokio::spawn(async move {
            run_server(listener, init_manager, room_manager).await;
        });

        let mut stream = tokio::net::TcpStream::connect(listener_addr)
            .await
            .expect("Failed to connect to server");

        write_lp_string(&mut stream, "test_token").await.unwrap();

        let mut buf = [0u8; 3];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"OK_");

        stream.write_all(&CURR_VERSION_BYTES).await.unwrap();


        stream.write_u8(INIT_MSG_ROOM_CREATE).await.unwrap();
        write_lp_string(&mut stream, "test_pw").await.unwrap();
        let deets = [0u8; 14];
        stream.write_all(&deets).await.unwrap();

        let room_id = read_lp_string(&mut stream).await.unwrap();
        println!("[Simple test] Room ID: {}", room_id);
        assert_eq!(room_id.len(), 6);

        let action_lim = stream.read_u32_le().await.unwrap();
        assert_eq!(action_lim, 0);
        // todo: read other stuff

        assert_eq!(room_mgr.read().await.rooms.read().await.len(), 1);
    }
}
