use std::sync::Arc;

use rand::Rng;
use tokio::{net::TcpListener, sync::RwLock};

use crate::managers::*;


#[derive(Default)]
pub struct ServerOpts {
    pub dump_macroblocks: bool,
}



pub async fn run_server(
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

pub async fn get_server_things(opts: ServerOpts) -> (TcpListener, Arc<InitializationManager>, Arc<RwLock<RoomManager>>) {
    // let { dump_macroblocks } = opts;
    let port = 19796;
    #[cfg(test)]
    let port = rand::thread_rng().gen_range(19797..30000);

    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("Failed to bind to port");
    log::info!("Server listening on {:?}", listener.local_addr().unwrap());

    let room_manager = Arc::new(RwLock::new(RoomManager::new(&opts)));
    let init_manager = Arc::new(InitializationManager::new(room_manager.clone()));
    (listener, init_manager, room_manager)
}
