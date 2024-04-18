use std::sync::Arc;
use std::time::{SystemTime};
use tokio::select;
use tokio::sync::mpsc::Receiver;
use tokio::sync::OnceCell;
use tokio::time::{self, Duration};

use futures::FutureExt;

use crate::map_actions::{MapAction, PlayerID};
use crate::{managers::*, DUMP_MBS};


pub async fn run_player_loop(player: Arc<Player>, room: Arc<Room>, mut action_rx: Receiver<ActionDescArc>) {
    log::debug!("Starting player loop for player {:?} / {:?}", player.get_name(), player.get_pid());
    room.send_player_room_details(&player).await;
    log::debug!("Sent player room details");
    log::debug!("Sync'd player with existing room actions");

    let loop_max_hz = 150.0;
    let loop_max_ms = 1000.0 / loop_max_hz;

    let player3 = player.clone();
    let player2 = player.clone();
    let room2 = room.clone();
    let room3 = room.clone();

    player3.sync_actions(room3.actions.read().await.as_ref()).await;

    // write loop
    tokio::spawn(async move {
        let p = player2;
        let room = room2;
        loop {
            match action_rx.recv().await {
                None => {
                    log::info!("[{}] Player loop ended for player {}", room.id_str, p.get_name());
                    break;
                },
                Some(action) => {
                    match Player::write_action(&mut *p.stream_w.write().await, &action.0, &action.1, action.2, &action.3).await {
                        Ok(_) => {}
                        Err(e) => {
                            log::error!("[{}] Error writing action to player {}: {:?}", room.id_str, p.get_name(), e);
                            room.player_left(&p).await;
                            log::info!("[{}] Player left: {}", room.id_str, p.get_name());
                            p.shutdown_err("error writing action").await;
                            log::debug!("[{}] Player {:?} shutdown", room.id_str, p.get_name());
                            action_rx.close();
                            // Handle player disconnect or error
                            break; // Ends the loop and thus the task for this player
                        }
                    }
                }
            }
        }
    });

    // read loop
    tokio::spawn(async move {
        loop {
            let start = SystemTime::now();
            let mut sync_actions = false;

            let player2 = player.clone();
            let readable = player.await_readable().fuse();
            let timeout = tokio::time::sleep(Duration::from_millis(loop_max_ms as u64)).fuse();

            // log::trace!("Player loop awaiting readable or timeout");

            futures::pin_mut!(readable, timeout);

            // log::trace!("Player loop awaiting readable or timeout");
            let mut ephemeral_action = None;
            let mut action = None;

            // log::debug!("player {:?} action: {:?}, getting lock and pushing", player.get_name(), action.0.get_type());
            // room.actions.lock().await.push(action.into());
            // log::debug!("Pushed action into room actions");
            select! {
                _ = readable => {
                    match player.read_map_msg().await {
                        Ok(a) => {
                            // log::trace!("Player {:?} action: {:?}", player.get_name(), action.get_type());
                            if *DUMP_MBS.get().unwrap_or(&false) {
                                log::debug!("[{}] player {:?} action: {:?}", room.id_str, player2.get_name(), a.get_type());
                                let a = a.clone();
                                tokio::spawn(async move {
                                    let _ = crate::map_actions::dump_macroblocks(a.clone(), player2.clone()).await;
                                });
                            }
                            let a = (a, player.get_pid().into(), SystemTime::now(), OnceCell::new());
                            if a.0.is_ephemeral() {
                                ephemeral_action = Some(a);
                            } else if a.0.is_ping() {
                                let _ = Player::write_action(&mut *player.stream_w.write().await, &MapAction::ServerStats(*TOTAL_PLAYERS.read().await), &player.get_pid().into(), SystemTime::now(), &Default::default()).await;
                            } else {
                                action = Some(a);
                                sync_actions = true;
                            }
                            // Sync actions to all players, consider optimizing this part
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

            if sync_actions || ephemeral_action.is_some() {
                // let players = room.players.read().await.clone();

                // non ephemeral actions
                if let Some(action) = action.take() {
                    log::debug!("[{}] player {:?} action: {:?}, pushing to room", room.id_str, player.get_name(), action.0.get_type());
                    match room.add_action(action).await {
                        Ok(_) => {}
                        Err(e) => {
                            log::error!("[{}] Error pushing action to room: {:?}", room.id_str, e);
                            player.shutdown_err("error pushing action to room").await;
                            room.player_left(&player).await;
                            log::info!("[{}] Player left: {:?}", room.id_str, player.get_name());
                            break;
                        }
                    };
                }

                if let Some(action) = ephemeral_action {
                    let action: Arc<_> = action.into();
                    // log::debug!("ephemeral action: players read lock");
                    let players = room.players.read().await.clone();
                    // log::debug!("ephemeral action: players read unlock");
                    tokio::spawn(async move {
                        // log::debug!("ephemeral action {:?}: writing to all players", &action.0.get_type());
                        for p in players.iter() {
                            let _ = p.action_tx.send(action.clone()).await;
                            // Player::write_action(&mut *p.stream_w.write().await, &action.0, &action.1, action.2, &action.3).await;
                        }
                        // log::debug!("ephemeral action {:?}: done writing to all players", &action.0.get_type());
                    });
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
        log::info!("[{}] Player loop ended for player {:?}", room.id_str, player.get_name());
    });
}
