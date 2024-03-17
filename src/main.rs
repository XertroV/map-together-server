use log::LevelFilter;
use managers::*;
use op_auth::*;
use simple_logger::SimpleLogger;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::RwLock};

mod msgs;
mod managers;
mod map_actions;
mod mt_room;
mod op_auth;
mod mt_codec;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    let logger = SimpleLogger::new()
        .with_level(LevelFilter::Trace)
        .with_colors(true);
    log::set_max_level(LevelFilter::Trace);
    log::set_boxed_logger(Box::new(logger)).unwrap();

    // Initialize your application configurations
    let nb_configs = init_op_config().await;
    log::info!("Configurations initialized: {}", nb_configs);

    let listener = TcpListener::bind("0.0.0.0:19796")
        .await
        .expect("Failed to bind to port");
    println!("Server listening on {:?}", listener.local_addr().unwrap());

    let room_manager = Arc::new(RwLock::new(RoomManager::new()));
    let init_manager = Arc::new(InitializationManager::new(room_manager.clone()));

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
