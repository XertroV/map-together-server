use std::sync::Arc;
use std::time::SystemTime;
use tokio::select;
use tokio::sync::mpsc::Receiver;
use tokio::sync::OnceCell;
use tokio::time::{self, Duration};

use futures::FutureExt;

use crate::map_actions::MapAction;
use crate::{managers::*, DUMP_MBS};

pub async fn run_player_loop(
    player: Arc<Player>,
    room: Arc<Room>,
    action_rx: Receiver<ActionDescArc>,
) {
    log::debug!(
        "Starting player loop for player {:?} / {:?}",
        player.get_name(),
        player.get_pid()
    );
    tokio::spawn(async move {
        let mut shutdown_rx = player.shutdown_rx.write().await.take().unwrap();
        // One owner supervises both directions. Dropping the losing futures
        // releases their socket locks before cleanup, including partial reads
        // and writes blocked on a peer that stopped reading. An idle writer
        // must not retain Player (and its own action_tx) after the reader exits.
        let error = select! {
            _ = shutdown_rx.recv() => None,
            error = async {
                room.send_player_room_details(&player).await;
                log::debug!("Sent player room details");
                player.sync_actions(room.actions.read().await.as_ref()).await;
                log::debug!("Sync'd player with existing room actions");
                select! {
                    error = write_player_actions(&player, &room, action_rx) => error,
                    error = read_player_actions(&player, &room) => error,
                }
            } => error,
        };

        if let Some(error) = error {
            player.shutdown_err(error).await;
        } else {
            player.shutdown().await;
        }
        room.player_left(&player).await;
        log::info!(
            "[{}] Player loop ended for player {:?}",
            room.id_str,
            player.get_name()
        );
    });
}

async fn write_player_actions(
    p: &Arc<Player>,
    room: &Arc<Room>,
    mut action_rx: Receiver<ActionDescArc>,
) -> Option<&'static str> {
    loop {
        match action_rx.recv().await {
            None => {
                log::info!(
                    "[{}] Player write loop ended for player {}",
                    room.id_str,
                    p.get_name()
                );
                return None;
            }
            Some(action) => {
                // Never relay a message type this client is too old to
                // parse — old clients crash on unknown message types.
                if p.proto_version < action.0.min_proto_version() {
                    continue;
                }
                match Player::write_action(
                    &mut *p.stream_w.write().await,
                    &action.0,
                    &action.1,
                    action.2,
                    &action.3,
                )
                .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        log::error!(
                            "[{}] Error writing action to player {}: {:?}",
                            room.id_str,
                            p.get_name(),
                            e
                        );
                        return Some("error writing action");
                    }
                }
            }
        }
    }
}

async fn read_player_actions(player: &Arc<Player>, room: &Arc<Room>) -> Option<&'static str> {
    let loop_max_hz = 150.0;
    let loop_max_ms = 1000.0 / loop_max_hz;
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
                            // copy the count first: inlining the read guard
                            // into write_action holds it across the socket
                            // write, blocking the room_mgr_loop's writer if
                            // this peer stops reading.
                            let nb_players = *TOTAL_PLAYERS.read().await;
                            let _ = Player::write_action(&mut *player.stream_w.write().await, &MapAction::ServerStats(nb_players), &player.get_pid().into(), SystemTime::now(), &Default::default()).await;
                        } else {
                            action = Some(a);
                            sync_actions = true;
                        }
                        // Sync actions to all players, consider optimizing this part
                    },
                    Err(e) => {
                        // EOF is a normal client quit, not an error; and
                        // never Debug-dump the whole Player struct.
                        match &e {
                            StreamErr::Io(io_e) if io_e.kind() == std::io::ErrorKind::UnexpectedEof => {
                                log::info!("[{}] Player {:?} disconnected (EOF)", room.id_str, player.get_name());
                                return None;
                            }
                            _ => {
                                log::error!("[{}] Error reading map message from player {:?} ({}): {}", room.id_str, player.get_name(), player.get_pid(), e);
                            }
                        }
                        return Some("error reading map message");
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
                log::debug!(
                    "[{}] player {:?} action: {:?}, pushing to room",
                    room.id_str,
                    player.get_name(),
                    action.0.get_type()
                );
                match room.add_action(action).await {
                    Ok(_) => {}
                    Err(e) => {
                        log::error!("[{}] Error pushing action to room: {:?}", room.id_str, e);
                        return Some("error pushing action to room");
                    }
                };
            }

            if let Some(action) = ephemeral_action {
                let action: Arc<_> = action.into();
                let players = room.players.read().await.clone();
                // Inline, not spawned: spawned fan-out tasks can run out of
                // order and deliver stale cursor/vehicle positions after
                // newer ones. try_send so one player's full queue can't
                // stall this read loop; dropping an ephemeral is harmless.
                for p in players.iter() {
                    let _ = p.action_tx.try_send(action.clone());
                }
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
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use std::sync::Weak;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn connection() -> (TcpStream, Arc<Room>, Weak<Player>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        socket2::SockRef::from(&server)
            .set_send_buffer_size(4096)
            .unwrap();
        socket2::SockRef::from(&client)
            .set_recv_buffer_size(4096)
            .unwrap();
        let (mut player, rx) = Player::new(
            crate::op_auth::TokenResp {
                account_id: "lifetime-test".into(),
                display_name: "lifetime-test".into(),
                token_time: 0,
            },
            server,
        );
        player.proto_version = 6;
        let player = Arc::new(player);
        let weak_player = Arc::downgrade(&player);
        let (room_tx_action, mut room_rx_action) = tokio::sync::mpsc::channel(52000);
        // Control the manager's room lifetime here instead of starting its
        // intentional 30-minute expiry timer. The real player loops and
        // player_left cleanup still run against the normal room ownership.
        let room = Arc::new(Room {
            id: 1,
            id_str: "test01".into(),
            deets: crate::msgs::RoomCreationDeets {
                password: String::new(),
                action_rate_limit: 0,
                map_size: [48, 255, 48],
                map_base: 128,
                base_car: 0,
                rules_flags: 0,
                item_max_size: 0,
                player_limit: 8,
            },
            players: vec![player.clone()].into(),
            owner: player.clone().into(),
            admins: vec![player.clone()].into(),
            mods: vec![].into(),
            actions: vec![].into(),
            has_expired: OnceCell::new(),
            player_limit: 8.into(),
            room_tx_action,
            action_bytes_sent: 0.into(),
        });
        tokio::spawn(async move { while room_rx_action.recv().await.is_some() {} });
        run_player_loop(player, room.clone(), rx).await;
        time::timeout(Duration::from_secs(2), async {
            let id_len = client.read_u16_le().await.unwrap();
            let mut details = vec![0; id_len as usize + 16];
            client.read_exact(&mut details).await.unwrap();
        })
        .await
        .expect("room handshake");
        (client, room, weak_player)
    }

    async fn assert_released(room: Arc<Room>, player: Weak<Player>) {
        time::timeout(Duration::from_secs(3), async {
            while !room.players.read().await.is_empty() {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("connection cleanup must remove the player");
        let weak_room = Arc::downgrade(&room);
        // Model removal of the empty, expired room from the room manager.
        drop(room);
        time::timeout(Duration::from_secs(3), async {
            while player.upgrade().is_some() || weak_room.upgrade().is_some() {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("disconnected player/socket and expired room must be released");
    }

    #[tokio::test]
    async fn eof_releases_idle_writer_and_room() {
        for _ in 0..3 {
            let (client, room, player) = connection().await;
            drop(client);
            assert_released(room, player).await;
        }
    }

    async fn begin_partial_frame(client: &mut TcpStream, player: &Arc<Player>) {
        client
            .write_u8(crate::map_actions::MAPPING_MSG_VEHICLE_SAMPLE)
            .await
            .unwrap();
        client.write_u32_le(64).await.unwrap();
        client.write_u8(0).await.unwrap();
        time::timeout(Duration::from_secs(2), async {
            loop {
                if player.stream_r.try_write().is_err() {
                    time::sleep(Duration::from_millis(50)).await;
                    if player.stream_r.try_write().is_err() {
                        break;
                    }
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reader must be blocked inside the partial frame");
    }

    #[tokio::test]
    async fn shutdown_cancels_partially_read_frame() {
        let (mut client, room, player) = connection().await;
        let p = player.upgrade().unwrap();
        begin_partial_frame(&mut client, &p).await;
        p.shutdown().await;
        drop(p);
        assert_released(room, player).await;
        drop(client);
    }

    #[tokio::test]
    async fn writer_error_cancels_partially_read_frame() {
        let (mut client, room, player) = connection().await;
        let p = player.upgrade().unwrap();
        begin_partial_frame(&mut client, &p).await;
        // Close only the server write half, without signalling shutdown or
        // closing the client. The queued write must fail and cancel the read.
        p.stream_w.write().await.shutdown().await.unwrap();
        p.action_tx
            .try_send(Arc::new((
                MapAction::ServerStats(0),
                "lifetime-test".into(),
                SystemTime::now(),
                OnceCell::new(),
            )))
            .unwrap();
        drop(p);
        assert_released(room, player).await;
        drop(client);
    }

    #[tokio::test]
    async fn shutdown_cancels_blocked_writer() {
        let (client, room, player) = connection().await;
        let p = player.upgrade().unwrap();
        let action = Arc::new((
            MapAction::VehicleSample(vec![0; 512]),
            "lifetime-test".into(),
            SystemTime::now(),
            OnceCell::new(),
        ));
        for _ in 0..4096 {
            p.action_tx.try_send(action.clone()).unwrap();
        }
        time::timeout(Duration::from_secs(3), async {
            loop {
                if p.stream_w.try_write().is_err() {
                    time::sleep(Duration::from_millis(50)).await;
                    if p.stream_w.try_write().is_err() {
                        break;
                    }
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("writer must block on a client that is not reading");
        time::timeout(Duration::from_secs(3), p.shutdown())
            .await
            .expect("shutdown must not wait for the blocked socket write");
        drop(p);
        assert_released(room, player).await;
        drop(client);
    }
}
