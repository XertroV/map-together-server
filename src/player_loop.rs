use std::sync::Arc;
use std::time::{SystemTime};
use tokio::select;
use tokio::sync::OnceCell;
use tokio::time::{self, Duration};

use futures::FutureExt;

use crate::managers::*;


pub async fn run_player_loop(player: Arc<Player>, room: Arc<Room>) {
    log::debug!("Starting player loop for player {:?} / {:?}", player.get_name(), player.get_pid());
    room.send_player_room_details(&player).await;
    log::debug!("Sent player room details");
    player.sync_actions(room.actions.read().await.as_ref()).await;
    log::debug!("Sync'd player with existing room actions");

    let loop_max_hz = 150.0;
    let loop_max_ms = 1000.0 / loop_max_hz;

    tokio::spawn(async move {
        loop {
            let start = SystemTime::now();
            let mut sync_actions = false;

            let readable = player.await_readable().fuse();
            let timeout = tokio::time::sleep(Duration::from_millis(loop_max_ms as u64)).fuse();

            // log::trace!("Player loop awaiting readable or timeout");

            futures::pin_mut!(readable, timeout);

            // log::trace!("Player loop awaiting readable or timeout");

            select! {
                _ = readable => {
                    match player.read_map_msg().await {
                        Ok(action) => {
                            log::trace!("Player {:?} action: {:?}", player.get_name(), action.get_type());
                            let action = (action, player.get_pid().into(), SystemTime::now(), OnceCell::new());
                            room.actions.write().await.push(action);
                            // Sync actions to all players, consider optimizing this part
                            sync_actions = true;
                        },
                        Err(e) => {
                            log::error!("Error reading map message from player {:?}: {:?}", player, e);
                            player.shutdown_err("error reading map message").await;
                            // Handle player disconnect or error
                            room.player_left(&player).await;
                            log::info!("Player left: {:?}", player.get_name());
                            break; // Ends the loop and thus the task for this player
                        },
                    }
                },
                _ = timeout => {
                    // Timeout occurred, no action necessary here since loop will continue
                    // log::trace!("Player loop timeout for player {:?}", player.get_name());
                },
            }

            if sync_actions {
                let actions = room.actions.read().await;
                for p in room.players.read().await.iter() {
                    p.sync_actions(&actions).await;
                }
            }

            // Handle loop timing to maintain the loop frequency
            if let Ok(elapsed) = start.elapsed() {
                let elapsed_ms = elapsed.as_millis() as f64;
                if elapsed_ms < loop_max_ms {
                    let sleep_for = (loop_max_ms - elapsed_ms).round();
                    time::sleep(Duration::from_millis(sleep_for as u64)).await;
                }
            }
        }
        log::info!("Player loop ended for player {:?}", player.get_name());
    });
}
