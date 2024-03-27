use futures::Future;
use log::LevelFilter;
use managers::*;
use op_auth::*;
use simple_logger::SimpleLogger;
use std::sync::{Arc, OnceLock};
use tokio::{net::TcpListener, sync::{RwLock}};

// #[cfg(test)]
// #[macro_use]
// extern quickcheck;

mod msgs;
mod managers;
mod map_actions;
mod mt_room;
mod op_auth;
mod mt_codec;
mod player_loop;
// #[cfg(test)]
mod client_bot;


// pub DUMP_MBS: OnceCell<bool> = OnceCell::new();
pub static DUMP_MBS: OnceLock<bool> = OnceLock::new();

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

#[derive(Default)]
struct ServerOpts {
    dump_macroblocks: bool,
}


async fn run_server(
    listener: TcpListener,
    init_manager: Arc<InitializationManager>,
    room_manager: Arc<RwLock<RoomManager>>,
) {
    let room_mgr_c = room_manager.clone();
    tokio::spawn(async move {
        room_mgr_c.read().await.room_mgr_loop().await
    });

    loop {
        let (socket, _) = listener
            .accept()
            .await
            .expect("Failed to accept connection");
        let init_manager_clone = init_manager.clone();

        // Spawn a new task for each connection
        tokio::spawn(async move {
            init_manager_clone.initialize_connection(socket).await;
        });
    }
}

async fn get_server_things(opts: ServerOpts) -> (TcpListener, Arc<InitializationManager>, Arc<RwLock<RoomManager>>) {
    // let { dump_macroblocks } = opts;
    let listener = TcpListener::bind("0.0.0.0:19796")
        .await
        .expect("Failed to bind to port");
    log::info!("Server listening on {:?}", listener.local_addr().unwrap());

    let room_manager = Arc::new(RwLock::new(RoomManager::new(&opts)));
    let init_manager = Arc::new(InitializationManager::new(room_manager.clone()));
    (listener, init_manager, room_manager)
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
        let listener_addr = listener.local_addr().unwrap();
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

        stream.write_all(&[0xFF as u8, 0x03, 0x80]).await.unwrap();


        stream.write_u8(INIT_MSG_ROOM_CREATE).await.unwrap();
        write_lp_string(&mut stream, "test_pw").await.unwrap();
        let deets = [0u8; 14];
        stream.write_all(&deets).await.unwrap();

        let room_id = read_lp_string(&mut stream).await.unwrap();
        println!("Room ID: {}", room_id);
        assert_eq!(room_id.len(), 6);

        let action_lim = stream.read_u32_le().await.unwrap();
        assert_eq!(action_lim, 0);

        assert_eq!(room_mgr.read().await.rooms.read().await.len(), 1);
    }
}
