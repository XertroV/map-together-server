use std::collections::HashMap;
use futures::stream;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{OnceCell, RwLock};
use tokio::{select, time};

use futures::future::{self, Either};
use std::ops::DerefMut;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::map_actions::{
    MacroblockSpec, MapAction, PlayerID, SetSkinSpec, SkinSpec, WaypointSpec, MAPPING_MSG_DELETE, MAPPING_MSG_PLACE, MAPPING_MSG_PLAYER_CAMCURSOR, MAPPING_MSG_PLAYER_JOIN, MAPPING_MSG_PLAYER_LEAVE, MAPPING_MSG_PLAYER_VEHICLEPOS, MAPPING_MSG_RESYNC, MAPPING_MSG_SET_ACTION_LIMIT, MAPPING_MSG_SET_MAPNAME, MAPPING_MSG_SET_SKIN, MAPPING_MSG_SET_WAYPOINT
};
use crate::msgs::{
    generate_room_id, room_id_to_str, str_to_room_id, PlayerCamCursor, PlayerVehiclePos, RoomConnectionDeets, RoomCreationDeets, RoomMsg
};
use crate::mt_codec::{MTDecode, MTEncode};
use crate::player_loop::run_player_loop;
use crate::{check_token, TokenResp};

#[derive(Debug, Default)]
struct PlayerSync {
    last_sync: usize,
}

#[derive(Debug)]
pub struct Player {
    pub token_resp: TokenResp,
    pub stream_r: Arc<RwLock<OwnedReadHalf>>,
    pub stream_w: Arc<RwLock<OwnedWriteHalf>>,
    sync: RwLock<PlayerSync>,
}

impl Player {
    pub fn new(token_resp: TokenResp, stream: TcpStream) -> Self {
        let (r, w) = stream.into_split();
        Player {
            token_resp,
            stream_r: Arc::new(r.into()),
            stream_w: Arc::new(w.into()),
            sync: RwLock::new(PlayerSync::default()),
        }
    }

    pub fn get_name(&self) -> &str {
        &self.token_resp.display_name
    }

    pub fn get_pid(&self) -> &str {
        &self.token_resp.account_id
    }

    pub async fn sync_actions(
        &self,
        actions: &Vec<Arc<(MapAction, PlayerID, SystemTime, OnceCell<Vec<u8>>)>>,
    ) {
        let mut sync = self.sync.write().await;
        if sync.last_sync == actions.len() {
            return;
        }
        let new_actions = actions[sync.last_sync..].to_vec();
        log::trace!(
            "Syncing actions: {:?} for player {:?}",
            new_actions.len(),
            self.token_resp.account_id
        );
        let stream = self.stream_w.clone();
        let stream2 = stream.clone();
        sync.last_sync += new_actions.len();
        let stream_g = stream2.write().await;
        drop(sync);
        let name: String = self.get_name().into();
        tokio::spawn(async move {
            let mut stream = stream.write().await;
            for action in new_actions {
                Self::write_action(&mut *stream, &action.0, &action.1, action.2, &action.3)
                    .await;
                log::debug!(
                    "Wrote action: {:?} -> player {:?}",
                    action.0.get_type(),
                    name
                );
            }
        });
        drop(stream_g);
    }

    pub async fn write_action(
        mut stream: &mut OwnedWriteHalf,
        action: &MapAction,
        pid: &PlayerID,
        time: SystemTime,
        buf: &OnceCell<Vec<u8>>,
    ) {
        // let mut stream = &mut *self.stream.write().await;
        let mut r = Ok(());
        match action {
            MapAction::Place(mb) => {
                if buf.initialized() {
                    r = stream.write_all(&buf.get().unwrap()).await;
                } else {
                    let mut new_buf = vec![];
                    let mb_buf = mb.encode();
                    new_buf.push(MAPPING_MSG_PLACE);
                    new_buf.extend_from_slice(&(mb_buf.len() as u32).to_le_bytes());
                    log::debug!("Wrote MB buf len: 0x{:x}", mb_buf.len() as u32);
                    new_buf.extend_from_slice(&mb_buf);
                    write_lp_string_to_buf(&mut new_buf, &pid.0);
                    new_buf.extend_from_slice(
                        &(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                            .to_le_bytes(),
                    );
                    r = stream.write_all(&new_buf).await;
                    new_buf.shrink_to_fit();
                    buf.get_or_init(|| async move { new_buf }).await;
                }
            }
            MapAction::Delete(mb) => {
                if buf.initialized() {
                    r = stream.write_all(&buf.get().unwrap()).await;
                } else {
                    let mut new_buf = vec![];
                    let mb_buf = mb.encode();
                    new_buf.push(MAPPING_MSG_DELETE);
                    new_buf.extend_from_slice(&(mb_buf.len() as u32).to_le_bytes());
                    new_buf.extend_from_slice(&mb_buf);
                    write_lp_string_to_buf(&mut new_buf, &pid.0);
                    new_buf.extend_from_slice(
                        &(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                            .to_le_bytes(),
                    );
                    r = stream.write_all(&new_buf).await;
                    new_buf.shrink_to_fit();
                    buf.get_or_init(|| async move { new_buf }).await;
                }
            }
            MapAction::Resync() => {
                // don't send this to the client
                // r = stream.write_u8(MAPPING_MSG_RESYNC).await.unwrap();
            }
            MapAction::SetSkin(skin) => {
                let buf = skin.encode();
                r = stream.write_u8(MAPPING_MSG_SET_SKIN).await;
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
                write_lp_string_owh(&mut stream, &pid.0).await.unwrap();
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await
                    .unwrap();
            }
            MapAction::SetWaypoint(wp) => {
                let buf = wp.encode();
                r = stream.write_u8(MAPPING_MSG_SET_WAYPOINT).await;
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
                write_lp_string_owh(&mut stream, &pid.0).await.unwrap();
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await
                    .unwrap();
            }
            MapAction::SetMapName(name) => {
                r = stream.write_u8(MAPPING_MSG_SET_MAPNAME).await;
                write_lp_string_owh(&mut stream, name).await.unwrap();
                write_lp_string_owh(&mut stream, &pid.0).await.unwrap();
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await
                    .unwrap();
            }
            MapAction::PlayerJoin { name, account_id } => {
                let mut buf = vec![];
                write_lp_string_to_buf(&mut buf, name);
                r = stream.write_u8(MAPPING_MSG_PLAYER_JOIN).await;
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
                write_lp_string_owh(&mut stream, &account_id).await.unwrap();
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await
                    .unwrap();
            }
            MapAction::PlayerLeave { name, account_id } => {
                let mut buf = vec![];
                write_lp_string_to_buf(&mut buf, name);
                r = stream.write_u8(MAPPING_MSG_PLAYER_LEAVE).await;
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
                write_lp_string_owh(&mut stream, &account_id).await.unwrap();
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await
                    .unwrap();
            }
            MapAction::Admin_PromoteMod(pid) => todo!(),
            MapAction::Admin_DemoteMod(pid) => todo!(),
            MapAction::Admin_KickPlayer(pid) => todo!(),
            MapAction::Admin_BanPlayer(pid) => todo!(),
            MapAction::Admin_ChangeAdmin(pid) => todo!(),
            MapAction::Admin_SetActionLimit(limit) => {
                let mut buf = vec![];
                buf.extend(limit.to_le_bytes());
                stream.write_u8(MAPPING_MSG_SET_ACTION_LIMIT).await.unwrap();
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
                write_lp_string_owh(&mut stream, &pid.0).await.unwrap();
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await
                    .unwrap();
            }
            MapAction::PlayerCamCursor(cam_cursor) => {
                if buf.initialized() {
                    r = stream.write_all(&buf.get().unwrap()).await;
                } else {
                    let mut new_buf = vec![];
                    let mb_buf = cam_cursor.encode();
                    new_buf.push(MAPPING_MSG_PLAYER_CAMCURSOR);
                    new_buf.extend_from_slice(&(mb_buf.len() as u32).to_le_bytes());
                    // log::debug!("Wrote cam_cursor buf len: 0x{:x}", mb_buf.len() as u32);
                    new_buf.extend_from_slice(&mb_buf);
                    write_lp_string_to_buf(&mut new_buf, &pid.0);
                    new_buf.extend_from_slice(
                        &(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                            .to_le_bytes(),
                    );
                    r = stream.write_all(&new_buf).await;
                    new_buf.shrink_to_fit();
                    buf.get_or_init(|| async move { new_buf }).await;
                }
            },
            MapAction::VehiclePos(veh_pos) => {
                if buf.initialized() {
                    r = stream.write_all(&buf.get().unwrap()).await;
                } else {
                    let mut new_buf = vec![];
                    let mb_buf = veh_pos.encode();
                    new_buf.push(MAPPING_MSG_PLAYER_VEHICLEPOS);
                    new_buf.extend_from_slice(&(mb_buf.len() as u32).to_le_bytes());
                    // log::debug!("Wrote veh_pos buf len: 0x{:x}", mb_buf.len() as u32);
                    new_buf.extend_from_slice(&mb_buf);
                    write_lp_string_to_buf(&mut new_buf, &pid.0);
                    new_buf.extend_from_slice(
                        &(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                            .to_le_bytes(),
                    );
                    r = stream.write_all(&new_buf).await;
                    new_buf.shrink_to_fit();
                    buf.get_or_init(|| async move { new_buf }).await;
                }
            },
        }
        if r.is_err() {
            log::error!("Error writing action to player: {:?}", r);
        }
    }

    pub async fn write_pid_and_timestamp(&self, pid: &PlayerID, time: SystemTime) {
        let mut stream = self.stream_w.write().await;
        write_lp_string_owh(&mut stream, &pid.0).await.unwrap();
        stream
            .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
            .await
            .unwrap();
    }

    pub async fn read_room_msg(&mut self) -> Result<RoomMsg, StreamErr> {
        let mut _stream = self.stream_r.write().await;
        let mut stream = _stream.deref_mut();
        let msg_ty = stream.read_u8().await?;
        match msg_ty {
            1 => {
                // Room creation
                let password = read_lp_string_owh(&mut stream).await?;
                let action_rate_limit = stream.read_u32_le().await?;
                let map_size: [u8; 3] = [
                    stream.read_u8().await?,
                    stream.read_u8().await?,
                    stream.read_u8().await?,
                ];
                let map_base: u8 = stream.read_u8().await?;
                let base_car: u8 = stream.read_u8().await?;
                let rules_flags: u8 = stream.read_u8().await? & (0xFF ^ 3); // clear last 2 bits, disallow custom items and sweep all
                let item_max_size: u32 = stream.read_u32_le().await?;
                // Ok(format!("Room creation: password: {}, action_rate_limit: {}", password, action_rate_limit))
                Ok(RoomMsg::Create(RoomCreationDeets {
                    password,
                    action_rate_limit,
                    map_size,
                    map_base,
                    base_car,
                    rules_flags,
                    item_max_size,
                }))
            }
            2 => {
                // Room join
                let room_id = read_lp_string_owh(&mut stream).await?;
                let password = read_lp_string_owh(&mut stream).await?;
                // Ok(format!("Room join: room_id: {}, password: {}", room_id, password))
                Ok(RoomMsg::Join(RoomConnectionDeets { room_id, password }))
            }
            _ => Ok(RoomMsg::Unk(msg_ty, format!("Unknown message type"))),
        }
    }

    pub async fn await_readable(&self) -> Result<(), StreamErr> {
        let mut s = self.stream_r.write().await;
        s.readable().await?;
        let mut buf = [0u8; 4];
        s.peek(&mut buf).await?;
        // log::trace!("Peeked: {:?}", buf);
        Ok(())
    }

    pub async fn read_map_msg(&self) -> Result<MapAction, StreamErr> {
        // log::trace!("Awaiting readable map msg");
        self.await_readable().await?;
        // log::trace!("Reading map message");
        let mut _stream = self.stream_r.write().await;
        // log::trace!("Got write lock");
        let stream = _stream.deref_mut();

        let msg_ty = stream.read_u8().await?;
        let mut b = [0u8; 4];
        stream.peek(&mut b).await?;
        // log::trace!("Read message type: {} with len bytes: {:?}", msg_ty, b);
        // expect to read MapAction via MAPPING_MSG_* constants
        match msg_ty {
            MAPPING_MSG_PLACE => {
                // Place
                let len = stream.read_u32_le().await?;
                // log::trace!("Reading place message with len: {}", len);
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let mb = MacroblockSpec::decode(&buf)?;
                // log::trace!("Decoded place message: B:{:?} / I:{:?}", mb.blocks.len(), mb.items.len());
                Ok(MapAction::Place(mb))
            }
            MAPPING_MSG_DELETE => {
                // Delete
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let mb = MacroblockSpec::decode(&buf)?;
                Ok(MapAction::Delete(mb))
            }
            MAPPING_MSG_RESYNC => {
                // resync
                Ok(MapAction::Resync())
            }
            MAPPING_MSG_SET_SKIN => {
                // set skin
                let len = stream.read_u32_le().await?;
                // log::debug!("Reading skin message with len: {}", len);
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::SetSkin(SetSkinSpec::decode(&buf)?))
            }
            MAPPING_MSG_SET_WAYPOINT => {
                // set waypoint
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::SetWaypoint(WaypointSpec::decode(&buf)?))
            }
            MAPPING_MSG_SET_MAPNAME => {
                // set map name
                Ok(MapAction::SetMapName(read_lp_string_owh(stream).await?))
            }
            // MAPPING_MSG_PLAYER_JOIN => {
            //     // player join
            //     let len = stream.read_u32_le().await?;
            //     let mut buf = vec![0u8; len as usize];
            //     stream.read_exact(&mut buf).await?;
            //     let name = slice_to_lp_string(&buf[4..])?;
            //     let account_id = slice_to_lp_string(&buf[4 + 2 + name.len()..])?;
            //     Ok(MapAction::PlayerJoin { name, account_id })
            // }
            // MAPPING_MSG_PLAYER_LEAVE => {
            //     // player leave
            //     let len = stream.read_u32_le().await?;
            //     let mut buf = vec![0u8; len as usize];
            //     stream.read_exact(&mut buf).await?;
            //     let name = slice_to_lp_string(&buf[4..])?;
            //     let account_id = slice_to_lp_string(&buf[4 + 2 + name.len()..])?;
            //     Ok(MapAction::PlayerLeave { name, account_id })
            // }
            MAPPING_MSG_PLAYER_CAMCURSOR => {
                // player cam cursor
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::PlayerCamCursor(PlayerCamCursor::decode(&buf)?))
            }
            MAPPING_MSG_PLAYER_VEHICLEPOS => {
                // player vehicle pos
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::VehiclePos(PlayerVehiclePos::decode(&buf)?))
            }
            MAPPING_MSG_SET_ACTION_LIMIT => {
                // set action limit
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let limit = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                Ok(MapAction::Admin_SetActionLimit(limit))
            }
            _ => Err(StreamErr::InvalidData(format!(
                "Unknown message type: {}",
                msg_ty
            ))),
        }
    }

    pub async fn shutdown(&self) {
        let _ = self.stream_w.write().await.shutdown().await;
    }

    pub async fn shutdown_err(&self, arg: &str) {
        let mut stream = self.stream_w.write().await;
        let _ = stream.write_all(b"ERR").await;
        let _ = write_lp_string_owh(&mut stream, arg).await;
        drop(stream);
        self.shutdown().await;
    }

    // pub async fn run_loop(&self) {
    //     loop {
    //         let _ = self.await_readable().await;
    //         let action = self.read_map_msg().await;
    //         match action {
    //             Ok(action) => {
    //                 log::debug!("Read action: {:?}", action);
    //             }
    //             Err(e) => {
    //                 log::error!("Error1 reading map message: {:?}", e);
    //                 self.shutdown_err("error reading map message").await;
    //                 break;
    //             }
    //         }
    //     }
    // }
}

#[derive(Debug)]
pub struct Room {
    // Define room details here
    pub id: u64,
    pub id_str: String,
    pub deets: RoomCreationDeets,
    pub players: RwLock<Vec<Arc<Player>>>,
    pub owner: RwLock<Arc<Player>>,
    pub mods: RwLock<Vec<Arc<Player>>>,
    pub actions: RwLock<Vec<Arc<(MapAction, PlayerID, SystemTime, OnceCell<Vec<u8>>)>>>,
    pub has_expired: OnceCell<()>,
}

impl Room {
    pub async fn start_new_room(player: Player, deets: RoomCreationDeets) -> Arc<Room> {
        let id = generate_room_id();
        let id_str = room_id_to_str(id);
        let player_arc: Arc<_> = player.into();
        let room = Room {
            id,
            id_str,
            deets,
            players: vec![player_arc.clone()].into(),
            owner: player_arc.clone().into(),
            mods: vec![].into(),
            actions: vec![].into(),
            has_expired: OnceCell::new(),
        };
        let room = Arc::new(room);
        let room_clone = room.clone();
        log::info!(
            "Starting room: {:?} for {:?}",
            room.id_str,
            player_arc.get_pid()
        );
        tokio::spawn(async move {
            room_clone.run().await;
        });
        let room_clone = room.clone();
        room.add_player_joined_msg(&player_arc).await;
        tokio::spawn(async move { run_player_loop(player_arc, room_clone).await });
        log::info!("Room started: {:?}", room.id_str);
        room
    }

    pub async fn run(&self) {
        // check if we have no players every 10ms. If we have no players for stale_minutes (20 minutes), close the room.
        let mut no_players = 0;
        let wait_ms = 10;
        let stale_minutes = 20;
        loop {
            let players = self.players.read().await;
            if players.len() == 0 {
                no_players += wait_ms;
                if no_players > stale_minutes * 60 * 1000 {
                    log::info!(
                        "Room has no players for {} minutes, closing room: {:?}",
                        stale_minutes,
                        self.id_str
                    );
                    let _ = self.has_expired.set(());
                    break;
                }
            } else {
                no_players = 0;
            }
            drop(players);
            time::sleep(Duration::from_millis(wait_ms)).await;
        }
    }

    pub async fn add_player_via_join(
        &self,
        deets: RoomConnectionDeets,
        player: Player,
    ) -> Arc<Player> {
        // Add player to room
        if self.deets.password != deets.password {
            log::warn!("Invalid password for room: {}", deets.room_id);
            player.shutdown_err("invalid password").await;
            return player.into();
        }
        let player: Arc<_> = player.into();
        self.add_player_joined_msg(&player).await;
        self.players.write().await.push(player.clone());
        player
    }

    pub async fn add_player_joined_msg(&self, player: &Arc<Player>) {
        self.actions.write().await.push((
            MapAction::PlayerJoin {
                name: player.get_name().into(),
                account_id: player.get_pid().into(),
            },
            (player.get_pid()).into(),
            SystemTime::now(),
            OnceCell::new(),
        ).into());
    }

    pub async fn send_player_room_details(&self, player: &Player) {
        let mut stream = player.stream_w.write().await;
        let _ = write_lp_string_owh(&mut stream, &self.id_str).await;
        let _ = stream.write_u32_le(self.deets.action_rate_limit).await;
        let _ = stream.write_u8(self.deets.map_size[0]).await;
        let _ = stream.write_u8(self.deets.map_size[1]).await;
        let _ = stream.write_u8(self.deets.map_size[2]).await;
        let _ = stream.write_u8(self.deets.map_base).await;
        let _ = stream.write_u8(self.deets.base_car).await;
        let _ = stream.write_u8(self.deets.rules_flags).await;
        let _ = stream.write_u32_le(self.deets.item_max_size).await;
    }

    pub async fn player_left(&self, p: &Player) {
        log::debug!("getting players lock");
        let mut players = self.players.write().await;
        log::debug!("got players lock");
        players.retain(|x| x.token_resp.account_id != p.token_resp.account_id);
        let action = MapAction::PlayerLeave {
            name: p.token_resp.display_name.clone(),
            account_id: p.token_resp.account_id.clone(),
        };
        self.actions.write().await.push((
            action,
            (&p.token_resp.account_id).into(),
            SystemTime::now(),
            OnceCell::new(),
        ).into());
        log::debug!("got actions lock");
    }
}

pub struct RoomManager {
    // Track active rooms
    pub rooms: RwLock<HashMap<u64, Arc<Room>>>, // Example with room ID as key
}

impl RoomManager {
    pub fn new() -> Self {
        RoomManager {
            rooms: HashMap::new().into(),
        }
    }

    pub async fn room_mgr_loop(&self) {
        loop {
            let rooms = self.rooms.read().await;
            let mut to_remove = vec![];
            for (id, room) in rooms.iter() {
                if room.has_expired.initialized() {
                    to_remove.push(*id);
                    log::info!("Room expired: {:?}", room.id_str);
                }
            }
            drop(rooms);
            if to_remove.len() > 0 {
                let mut rooms = self.rooms.write().await;
                for id in to_remove {
                    rooms.remove(&id);
                }
                drop(rooms);
            }
            time::sleep(Duration::from_secs(10)).await;
        }
    }

    pub async fn manage_room(&self, mut player: Player) {
        // Room management logic here
        log::debug!(
            "Player connected: {:?} / {:?}",
            player.get_name(),
            player.get_pid()
        );
        let mut rx = player.stream_r.write().await;
        let mut ver_buf = [0u8; 4];
        match rx.peek(&mut ver_buf).await {
            Err(e) => {
                log::error!("Error reading room message: {:?}", e);
                player.shutdown_err("error reading room message").await;
                return;
            }
            Ok(_) => {
                // version flag 0xFF and version number with high bit set as flag.
                if ver_buf[0] != 0xFF || ver_buf[1] != 0x03 && ver_buf[2] != 0x80 {
                    log::warn!("\\$f40Invalid version bytes: 0x {:x} {:x} {:x}", ver_buf[0], ver_buf[1], ver_buf[2]);
                    write_lp_string_owh(&mut *player.stream_w.write().await, "\\$s\\$f84 Invalid version: please update.\n").await.unwrap();
                    player.shutdown_err("invalid version byte").await;
                    return;
                }
            }
        }

        // Consume version byte flag
        let _ = rx.read_u8().await.unwrap();
        // Consume version number
        let _ = rx.read_u16().await.unwrap();
        drop(rx);

        // read commands from player.stream
        match player.read_room_msg().await {
            Ok(msg) => {
                log::debug!("RoomMsg: {:?}", msg);
                match msg {
                    RoomMsg::Create(deets) => {
                        log::debug!("RoomCreationDeets: {:?}", deets);
                        // Create room
                        let room: Arc<Room> = Room::start_new_room(player, deets).await;
                        let room_id = room.id_str.clone();
                        log::debug!("Adding room: {:?}", room.id_str);
                        self.rooms.write().await.insert(room.id, room);
                        log::debug!("Added room: {:?}", room_id);
                    }
                    RoomMsg::Join(deets) => {
                        log::debug!("RoomConnectionDeets: {:?}", deets);
                        // Join room
                        let rooms = self.rooms.read().await;
                        let room = match rooms.get(&str_to_room_id(&deets.room_id).unwrap()) {
                            Some(room) => room,
                            None => {
                                log::warn!("Room not found: {}", deets.room_id);
                                player.shutdown_err("room not found").await;
                                return;
                            }
                        };
                        let p = room.add_player_via_join(deets, player).await;
                        run_player_loop(p, room.clone()).await;
                    }
                    RoomMsg::Unk(ty, msg) => {
                        log::warn!("Unknown message type: {} - {}", ty, msg);
                        player.shutdown_err("unknown message type").await;
                        return;
                    }
                }
            }
            Err(e) => {
                log::error!("Error reading room message: {:?}", e);
                player.shutdown_err("error reading room message").await;
            }
        }
    }
}

pub struct InitializationManager {
    pub room_manager: Arc<RwLock<RoomManager>>,
}

impl InitializationManager {
    pub fn new(room_manager: Arc<RwLock<RoomManager>>) -> Self {
        InitializationManager { room_manager }
    }

    pub async fn initialize_connection(&self, mut stream: TcpStream) {
        let peer = stream.peer_addr().expect("to get peer addr");
        log::info!("Initializing connection from: {:?}", peer);

        let token = read_lp_string(&mut stream).await;

        if let Err(e) = token {
            log::error!("Error reading token: {:?}", e);
            let _ = stream.write_all(b"ERR").await;
            let _ = write_lp_string(&mut stream, "malformed token").await;
            stream.shutdown().await.unwrap();
            return;
        }

        log::debug!("read token from {:?}", peer);

        let token = token.unwrap();
        log::debug!("Got token of length: {}", token.len());

        let token_resp = match check_token(&token, 521).await {
            Some(token_resp) => token_resp,
            None => {
                log::warn!("Token not verified!");
                let _ = stream.write_all(b"ERR").await;
                let _ = write_lp_string(&mut stream, "auth token verification failed").await;
                stream.shutdown().await.unwrap();
                return;
            }
        };
        log::info!("TokenResp: {:?}", token_resp);
        let _ = stream.write_all(b"OK_").await;

        let player = Player::new(token_resp, stream);

        // Hand off to Room Manager
        self.room_manager.read().await.manage_room(player).await;
    }
}

pub async fn read_lp_string(stream: &mut TcpStream) -> Result<String, StreamErr> {
    let mut buf = [0u8; 2];
    // log::info!("About to read string len");
    // match stream.peek(&mut buf).await {
    //     Ok(_) => { log::info!("peeked: {:?}", buf); },
    //     Err(e) => {
    //         log::error!("Error peeking: {:?}", e);
    //         return Err(StreamErr::Io(e));
    //     }
    // }
    stream.read_exact(&mut buf).await?;
    let len = u16::from_le_bytes(buf) as usize;
    // log::info!("Reading string of length: {}", len);
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    // log::info!("Read string: {:?}", buf);
    Ok(String::from_utf8(buf)?)
}

pub async fn read_lp_string_owh(stream: &mut OwnedReadHalf) -> Result<String, StreamErr> {
    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await?;
    let len = u16::from_le_bytes(buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(String::from_utf8(buf)?)
}

pub fn slice_to_lp_string(buf: &[u8]) -> Result<String, StreamErr> {
    if buf.len() < 2 {
        return Err(StreamErr::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "buffer too small",
        )));
    }
    let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;

    if len > buf.len() - 2 {
        return Err(StreamErr::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("buffer does not contain enough data: {}", len),
        )));
    }
    Ok(String::from_utf8_lossy(&buf[2..(len + 2)]).into_owned())
}

pub async fn write_lp_string(stream: &mut TcpStream, s: &str) -> Result<(), StreamErr> {
    let len = s.len() as u16;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(s.as_bytes()).await?;
    Ok(())
}
pub async fn write_lp_string_owh(stream: &mut OwnedWriteHalf, s: &str) -> Result<(), StreamErr> {
    let len = s.len() as u16;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(s.as_bytes()).await?;
    Ok(())
}

pub fn write_lp_string_to_buf(buf: &mut Vec<u8>, s: &str) {
    let len = s.len() as u16;
    let buf_pre_len = buf.len();
    // log::debug!("Writing string to buf: {} / {:?}", len, s);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
    let buf_post_len = buf.len();
    // log::debug!("Wrote string to buf: {:?}", &buf[buf_pre_len..buf_post_len]);
}

#[derive(Debug)]
pub enum StreamErr {
    Io(io::Error),
    Utf8(std::string::FromUtf8Error),
    InvalidData(String),
}

impl From<io::Error> for StreamErr {
    fn from(e: io::Error) -> Self {
        StreamErr::Io(e)
    }
}

impl From<std::string::FromUtf8Error> for StreamErr {
    fn from(e: std::string::FromUtf8Error) -> Self {
        StreamErr::Utf8(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_room_id() {
        let id = generate_room_id();
        let id_str = room_id_to_str(id);
        let id2 = str_to_room_id(&id_str).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn test_room_id_str() {
        let id_str = "YjERAS";
        let id = str_to_room_id(id_str).unwrap();
        let id2 = room_id_to_str(id);
        assert_eq!(id_str, id2);
    }
}
