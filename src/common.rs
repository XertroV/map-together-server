use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use socket2::{SockRef, TcpKeepalive};
use tokio::{net::TcpListener, sync::RwLock};

use crate::managers::*;


#[derive(Default)]
pub struct ServerOpts {
    pub dump_macroblocks: bool,
    /// When set, exit(0) after this many minutes with zero connected players
    /// so a supervisor (systemd Restart=always) restarts us on a fresh binary.
    /// Rooms are memory-only and are LOST on exit — leave unset in production.
    pub exit_when_idle_mins: Option<u64>,
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

    // Kernel-level keepalive so peers that vanish (NAT timeout, power loss)
    // error out of blocked reads/writes in ~2 min instead of holding a room
    // slot forever (observed live: established conns idle for 45+ days).
    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15))
        .with_retries(4);

    loop {
        // A transient accept error (e.g. EMFILE under a connection flood) must
        // not kill the accept loop — that would take the whole server down.
        let (socket, _) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                log::error!("Error accepting connection: {:?}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if let Err(e) = SockRef::from(&socket).set_tcp_keepalive(&keepalive) {
            log::warn!("Failed to set TCP keepalive: {:?}", e);
        }
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
