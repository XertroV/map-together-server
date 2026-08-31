use std::collections::HashMap;
use std::error::Error;
use std::fmt::Display;
use futures::stream;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::SendError;
use tokio::sync::{Mutex, OnceCell, RwLock};
use tokio::{select, time};
use tokio::sync::mpsc::{self, channel, Receiver, Sender};

use lazy_static::lazy_static;
use futures::future::{self, Either};
use std::ops::DerefMut;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::map_actions::{
    MacroblockSpec, MapAction, PlayerID, SetSkinSpec, SkinSpec, WaypointSpec, MAPPING_MSG_ALERT_STATUS_TO_ALL, MAPPING_MSG_BAN_PLAYER, MAPPING_MSG_CHANGE_ADMIN, MAPPING_MSG_CHAT_MSG, MAPPING_MSG_DELETE, MAPPING_MSG_DEMOTE_MOD, MAPPING_MSG_KICK_PLAYER, MAPPING_MSG_PING, MAPPING_MSG_PLACE, MAPPING_MSG_PLAYER_CAMCURSOR, MAPPING_MSG_PLAYER_JOIN, MAPPING_MSG_PLAYER_LEAVE, MAPPING_MSG_PLAYER_TEST_MODE, MAPPING_MSG_PLAYER_VEHICLEPOS, MAPPING_MSG_PROMOTE_MOD, MAPPING_MSG_RESYNC, MAPPING_MSG_SERVER_STATS, MAPPING_MSG_SET_ACTION_LIMIT, MAPPING_MSG_SET_MAPNAME, MAPPING_MSG_SET_ROOM_PLAYER_LIMIT, MAPPING_MSG_SET_SKIN, MAPPING_MSG_SET_VARIABLE, MAPPING_MSG_SET_WAYPOINT
};
use crate::msgs::{
    generate_room_id, room_id_to_str, str_to_room_id, PlayerCamCursor, PlayerTestMode, PlayerVehiclePos, RoomConnectionDeets, RoomCreationDeets, RoomMsg
};
use crate::mt_codec::{MTDecode, MTEncode};
use crate::player_loop::run_player_loop;
use crate::op_auth::{check_token, TokenResp};
use crate::{ServerOpts, XERTROV_WSID};

pub type ActionDesc = (MapAction, PlayerID, SystemTime, OnceCell<Vec<u8>>);
pub type ActionDescArc = Arc<ActionDesc>;

/// Protocol v5: no environment support; `map_base` env bits are always zero.
pub const VERSION_BYTES_V5: [u8; 3] = [0xFF as u8, 0x05, 0x80];
/// Protocol v6: `map_base` bits 2-4 carry the map environment (see
/// `RoomCreationDeets::map_env`), and adds the ephemeral `PlayerTestMode`
/// message (test-mode enter/leave + car-skin url), relayed to v6+ clients
/// only (see the `min_proto_version` gate in `run_player_loop`).
pub const VERSION_BYTES_V6: [u8; 3] = [0xFF as u8, 0x06, 0x80];
/// What our own clients/bots send. Older versions are still accepted on connect;
/// see `client_proto_version` and the env gate in `manage_room`.
pub const CURR_VERSION_BYTES: [u8; 3] = VERSION_BYTES_V6;

/// Map a client's version bytes to a protocol version number, or None if unsupported.
pub fn client_proto_version(ver_buf: &[u8; 3]) -> Option<u8> {
    match *ver_buf {
        VERSION_BYTES_V5 => Some(5),
        VERSION_BYTES_V6 => Some(6),
        _ => None,
    }
}

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
    pub action_tx: Sender<ActionDescArc>,
    pub shutdown_tx: Sender<()>,
    pub shutdown_rx: RwLock<Option<Receiver<()>>>,
    /// Negotiated protocol version; set in `manage_room` before the player
    /// joins a room (0 until then). Gates which message types we relay.
    pub proto_version: u8,
}

impl Player {
    pub fn new(token_resp: TokenResp, stream: TcpStream) -> (Self, Receiver<ActionDescArc>) {
        let (r, w) = stream.into_split();
        let (action_tx, action_rx) = mpsc::channel(51000);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        (Player {
            token_resp,
            stream_r: Arc::new(r.into()),
            stream_w: Arc::new(w.into()),
            sync: RwLock::new(PlayerSync::default()),
            action_tx,
            shutdown_tx,
            shutdown_rx: RwLock::new(Some(shutdown_rx)),
            proto_version: 0,
        }, action_rx)
    }

    pub fn get_name(&self) -> &str {
        &self.token_resp.display_name
    }

    pub fn get_pid(&self) -> &str {
        &self.token_resp.account_id
    }

    /// Queue any not-yet-synced room actions onto this player's write channel.
    /// Sends inline (try_send is non-blocking) rather than via a spawned task:
    /// concurrently spawned tasks could interleave batches and deliver actions
    /// out of order, which corrupts map state on the client.
    pub async fn sync_actions(
        self: &Arc<Self>,
        actions: &Vec<Arc<(MapAction, PlayerID, SystemTime, OnceCell<Vec<u8>>)>>,
    ) {
        let mut sync = self.sync.write().await;
        if sync.last_sync >= actions.len() {
            return;
        }
        let new_actions = &actions[sync.last_sync..];
        sync.last_sync = actions.len();
        log::trace!(
            "Syncing actions: {:?} for player {:?}",
            new_actions.len(),
            self.token_resp.account_id
        );
        for action in new_actions {
            match self.action_tx.try_send(action.clone()) {
                Ok(_) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // A silently dropped action would desync this client for
                    // the rest of the session; force a reconnect instead.
                    // Shutdown via a task: stream_w may be held by a blocked
                    // write to this same (slow/dead) peer.
                    log::error!("Action queue full for player {:?} - disconnecting for resync", self.get_name());
                    let p = self.clone();
                    tokio::spawn(async move {
                        p.shutdown_err("action queue overflow, please reconnect").await;
                    });
                    break;
                }
                Err(e @ mpsc::error::TrySendError::Closed(_)) => {
                    log::error!("Error sending action to player {:?}: {:?} - dropping", self.get_name(), e);
                    break;
                }
            }
        }
    }

    pub async fn write_action(
        mut stream: &mut OwnedWriteHalf,
        action: &MapAction,
        pid: &PlayerID,
        time: SystemTime,
        buf: &OnceCell<Vec<u8>>,
    ) -> Result<(), StreamErr> {
        // let mut stream = &mut *self.stream.write().await;
        match action {
            MapAction::Place(mb) => {
                if buf.initialized() {
                    stream.write_all(&buf.get().unwrap()).await?;
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
                    stream.write_all(&new_buf).await?;
                    new_buf.shrink_to_fit();
                    buf.get_or_init(|| async move { new_buf }).await;
                }
            }
            MapAction::Delete(mb) => {
                if buf.initialized() {
                    stream.write_all(&buf.get().unwrap()).await?;
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
                    stream.write_all(&new_buf).await?;
                    new_buf.shrink_to_fit();
                    buf.get_or_init(|| async move { new_buf }).await;
                }
            }
            MapAction::Resync() => {
                // don't send this to the client
                // stream.write_u8(MAPPING_MSG_RESYNC).await.unwrap()?;
            }
            MapAction::SetSkin(skin) => {
                let buf = skin.encode();
                stream.write_u8(MAPPING_MSG_SET_SKIN).await?;
                stream.write_u32_le(buf.len() as u32).await?;
                stream.write_all(&buf).await?;
                write_lp_string_owh(&mut stream, &pid.0).await?;
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await?;
            }
            MapAction::SetWaypoint(wp) => {
                let buf = wp.encode();
                stream.write_u8(MAPPING_MSG_SET_WAYPOINT).await?;
                stream.write_u32_le(buf.len() as u32).await?;
                stream.write_all(&buf).await?;
                write_lp_string_owh(&mut stream, &pid.0).await?;
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await?;
            }
            MapAction::SetMapName(name) => {
                stream.write_u8(MAPPING_MSG_SET_MAPNAME).await?;
                write_lp_string_owh(&mut stream, name).await?;
                write_lp_string_owh(&mut stream, &pid.0).await?;
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await?;
            }
            MapAction::PlayerJoin { name, account_id } => {
                let mut buf = vec![];
                write_lp_string_to_buf(&mut buf, name);
                stream.write_u8(MAPPING_MSG_PLAYER_JOIN).await?;
                stream.write_u32_le(buf.len() as u32).await?;
                stream.write_all(&buf).await?;
                write_lp_string_owh(&mut stream, &account_id).await?;
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await?;
            }
            MapAction::PlayerLeave { name, account_id } => {
                let mut buf = vec![];
                write_lp_string_to_buf(&mut buf, name);
                stream.write_u8(MAPPING_MSG_PLAYER_LEAVE).await?;
                stream.write_u32_le(buf.len() as u32).await?;
                stream.write_all(&buf).await?;
                write_lp_string_owh(&mut stream, &account_id).await?;
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await?;
            }
            MapAction::Admin_PromoteMod(t_pid) => write_str_msg_full(stream, MAPPING_MSG_PROMOTE_MOD, &t_pid.0, pid, time).await?,
            MapAction::Admin_DemoteMod(t_pid) => write_str_msg_full(stream, MAPPING_MSG_DEMOTE_MOD, &t_pid.0, pid, time).await?,
            MapAction::Admin_KickPlayer(t_pid) => write_str_msg_full(stream, MAPPING_MSG_KICK_PLAYER, &t_pid.0, pid, time).await?,
            MapAction::Admin_BanPlayer(t_pid) => write_str_msg_full(stream, MAPPING_MSG_BAN_PLAYER, &t_pid.0, pid, time).await?,
            MapAction::Admin_ChangeAdmin(t_pid) => write_str_msg_full(stream, MAPPING_MSG_CHANGE_ADMIN, &t_pid.0, pid, time).await?,
            MapAction::Admin_SetRoomPlayerLimit(limit) => write_u16_msg_full(stream, MAPPING_MSG_SET_ROOM_PLAYER_LIMIT, *limit, pid, time).await?,
            MapAction::Admin_SetActionLimit(limit) => write_u32_msg_full(stream, MAPPING_MSG_SET_ACTION_LIMIT, *limit, pid, time).await?,
            MapAction::Admin_AlertStatusToAll(msg) => write_str_msg_full(stream, MAPPING_MSG_ALERT_STATUS_TO_ALL, &msg, pid, time).await?,
            MapAction::Admin_SetVariable(k, v) => {
                let mut buf = vec![];
                write_lp_string_to_buf(&mut buf, k);
                write_lp_string_to_buf(&mut buf, v);
                stream.write_u8(MAPPING_MSG_SET_VARIABLE).await?;
                stream.write_u32_le(buf.len() as u32).await?;
                stream.write_all(&buf).await?;
                write_lp_string_owh(&mut stream, &pid.0).await?;
                stream
                    .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
                    .await?;
            }

            MapAction::PlayerCamCursor(cam_cursor) => {
                if buf.initialized() {
                    stream.write_all(&buf.get().unwrap()).await?;
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
                    stream.write_all(&new_buf).await?;
                    new_buf.shrink_to_fit();
                    buf.get_or_init(|| async move { new_buf }).await;
                }
            },
            MapAction::VehiclePos(veh_pos) => {
                if buf.initialized() {
                    stream.write_all(&buf.get().unwrap()).await?;
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
                    stream.write_all(&new_buf).await?;
                    new_buf.shrink_to_fit();
                    buf.get_or_init(|| async move { new_buf }).await;
                }
            },
            MapAction::ChatMsg(ty, msg) => {
                let mut buf = vec![];
                buf.push(*ty);
                write_lp_string_to_buf(&mut buf, msg);
                stream.write_u8(MAPPING_MSG_CHAT_MSG).await?;
                stream.write_u32_le(buf.len() as u32).await?;
                stream.write_all(&buf).await?;
                write_pid_and_timestamp(stream, pid, time).await?;
            },
            MapAction::Ping() => {
                // todo: respond with server stats
                // do nothing
            },
            MapAction::ServerStats(nb_players) => {
                let mut buf = vec![];
                buf.write_u32_le(*nb_players).await?;
                stream.write_u8(MAPPING_MSG_SERVER_STATS).await?;
                stream.write_u32_le(buf.len() as u32).await?;
                stream.write_all(&buf).await?;
                write_pid_and_timestamp(stream, pid, time).await?;
            }
            MapAction::PlayerTestMode(tm) => {
                write_encodable_msg_full(stream, MAPPING_MSG_PLAYER_TEST_MODE, tm, pid, time)
                    .await?
            }
        }
        Ok(())
    }

    // pub async fn write_pid_and_timestamp(&self, pid: &PlayerID, time: SystemTime) {
    //     let mut stream = self.stream_w.write().await;
    //     write_lp_string_owh(&mut stream, &pid.0).await.unwrap();
    //     stream
    //         .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
    //         .await
    //         .unwrap();
    // }

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
                let player_limit: u16 = stream.read_u16_le().await?;
                // Ok(format!("Room creation: password: {}, action_rate_limit: {}", password, action_rate_limit))
                Ok(RoomMsg::Create(RoomCreationDeets {
                    password,
                    action_rate_limit,
                    map_size,
                    map_base,
                    base_car,
                    rules_flags,
                    item_max_size,
                    player_limit
                }))
            }
            2 => {
                // Room join
                let room_id = read_lp_string_owh(&mut stream).await?;
                let password = read_lp_string_owh(&mut stream).await?;
                // Ok(format!("Room join: room_id: {}, password: {}", room_id, password))
                Ok(RoomMsg::Join(RoomConnectionDeets { room_id, password }))
            }
            _ => Ok(RoomMsg::Unk(msg_ty, format!("Unknown room message type"))),
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
                let len = read_checked_msg_u32_len(stream).await?;
                // log::trace!("Reading place message with len: {}", len);
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let mb = MacroblockSpec::decode(&buf)?;
                // log::trace!("Decoded place message: B:{:?} / I:{:?}", mb.blocks.len(), mb.items.len());
                Ok(MapAction::Place(mb))
            }
            MAPPING_MSG_DELETE => {
                // Delete
                let len = read_checked_msg_u32_len(stream).await?;
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
                let len = read_checked_msg_u32_len(stream).await?;
                // log::debug!("Reading skin message with len: {}", len);
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::SetSkin(SetSkinSpec::decode(&buf)?))
            }
            MAPPING_MSG_SET_WAYPOINT => {
                // set waypoint
                let len = read_checked_msg_u32_len(stream).await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::SetWaypoint(WaypointSpec::decode(&buf)?))
            }
            MAPPING_MSG_SET_MAPNAME => {
                // set map name
                Ok(MapAction::SetMapName(read_lp_string_owh(stream).await?))
            }
            MAPPING_MSG_PLAYER_CAMCURSOR => {
                // player cam cursor
                let len = read_checked_msg_u32_len(stream).await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::PlayerCamCursor(PlayerCamCursor::decode(&buf)?))
            }
            MAPPING_MSG_PLAYER_VEHICLEPOS => {
                // player vehicle pos
                let len = read_checked_msg_u32_len(stream).await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::VehiclePos(PlayerVehiclePos::decode(&buf)?))
            }
            MAPPING_MSG_PLAYER_TEST_MODE => {
                // test-mode enter/leave + car-skin url
                let len = read_checked_msg_u32_len(stream).await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::PlayerTestMode(PlayerTestMode::decode(&buf)?))
            }
            MAPPING_MSG_SET_ACTION_LIMIT => {
                // set action limit
                let len = read_checked_msg_u32_len(stream).await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                if buf.len() < 4 {
                    return Err(StreamErr::InvalidData(format!(
                        "SetActionLimit msg too short: {}",
                        buf.len()
                    )));
                }
                let limit = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                Ok(MapAction::Admin_SetActionLimit(limit))
            }
            MAPPING_MSG_SET_VARIABLE => {
                // set variable
                let len = read_checked_msg_u32_len(stream).await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                // layout: [4 unused bytes][lp key][lp value]; offsets from the
                // wire length prefix, not k.len() (lossy utf8 can change it).
                let rest = buf.get(4..).ok_or_else(|| {
                    StreamErr::InvalidData(format!("SetVariable msg too short: {}", buf.len()))
                })?;
                let k = slice_to_lp_string(rest)?;
                let k_wire_len = u16::from_le_bytes([rest[0], rest[1]]) as usize;
                let v_rest = rest.get(2 + k_wire_len..).ok_or_else(|| {
                    StreamErr::InvalidData("SetVariable msg missing value".to_string())
                })?;
                let v = slice_to_lp_string(v_rest)?;
                Ok(MapAction::Admin_SetVariable(k, v))
            }
            MAPPING_MSG_SET_ROOM_PLAYER_LIMIT => {
                // set room player limit
                let len = read_checked_msg_u32_len(stream).await?;
                if len != 4 {
                    return Err(StreamErr::InvalidData(format!(
                        "Invalid message length for set room player limit: {}",
                        len
                    )));
                }
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let limit = u16::from_le_bytes([buf[0], buf[1]]);
                Ok(MapAction::Admin_SetRoomPlayerLimit(limit))
            }
            MAPPING_MSG_CHAT_MSG => {
                // chat message
                let len = read_checked_msg_u32_len(stream).await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                if buf.is_empty() {
                    return Err(StreamErr::InvalidData("empty chat msg".to_string()));
                }
                let ty = buf[0];
                let msg = slice_to_lp_string(&buf[1..])?;
                log::debug!("Chat from {}: {}", self.get_name(), msg);
                Ok(MapAction::ChatMsg(ty, msg))
            }
            MAPPING_MSG_PING => {
                // ping -> respond with server stats
                Ok(MapAction::Ping())
            }
            MAPPING_MSG_SERVER_STATS => {
                // ignore this later but need to read message anyway
                let len = read_checked_msg_u32_len(stream).await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                log::debug!("Got server stats msg from {}??", self.get_name());
                if buf.len() < 4 {
                    return Err(StreamErr::InvalidData(format!(
                        "ServerStats msg too short: {}",
                        buf.len()
                    )));
                }
                Ok(MapAction::ServerStats(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])))
            }
            _ => Err(StreamErr::InvalidData(format!(
                "Unknown message type: {}",
                msg_ty
            ))),
        }
    }

    pub async fn shutdown(&self) {
        // wake the read loop (it selects on shutdown_rx); ignore full/closed
        let _ = self.shutdown_tx.try_send(());
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


pub async fn read_checked_msg_u32_len(stream: &mut OwnedReadHalf) -> Result<u32, StreamErr> {
    let len = stream.read_u32_le().await?;
    if len > 7000000 {
        return Err(StreamErr::InvalidData(format!("Message too large: {}", len)));
    }
    Ok(len)
}


pub async fn write_encodable_msg_full<T: MTEncode>(
    stream: &mut OwnedWriteHalf,
    msg_ty: u8,
    encodable: &T,
    pid: &PlayerID,
    time: SystemTime,
) -> Result<(), StreamErr> {
    let buf = encodable.encode();
    stream.write_u8(msg_ty).await?;
    stream.write_u32_le(buf.len() as u32).await?;
    stream.write_all(&buf).await?;
    write_pid_and_timestamp(stream, pid, time).await?;
    Ok(())
}


pub async fn write_u32_msg_full(
    stream: &mut OwnedWriteHalf,
    msg_ty: u8,
    val: u32,
    pid: &PlayerID,
    time: SystemTime,
) -> Result<(), StreamErr> {
    stream.write_u8(msg_ty).await?;
    stream.write_u32_le(4).await?;
    stream.write_u32_le(val).await?;
    write_pid_and_timestamp(stream, pid, time).await?;
    Ok(())
}

pub async fn write_u16_msg_full(
    stream: &mut OwnedWriteHalf,
    msg_ty: u8,
    val: u16,
    pid: &PlayerID,
    time: SystemTime,
) -> Result<(), StreamErr> {
    stream.write_u8(msg_ty).await?;
    stream.write_u32_le(2).await?;
    stream.write_u16_le(val).await?;
    write_pid_and_timestamp(stream, pid, time).await?;
    Ok(())
}


pub async fn write_str_msg_full(
    stream: &mut OwnedWriteHalf,
    msg_ty: u8,
    content: &str,
    my_pid: &PlayerID,
    time: SystemTime,
) -> Result<(), StreamErr> {
    let mut new_buf = vec![];
    write_lp_string_to_buf(&mut new_buf, content);
    stream.write_u8(msg_ty).await?;
    stream.write_u32_le(new_buf.len() as u32).await?;
    stream.write_all(&new_buf).await?;
    write_pid_and_timestamp(stream, my_pid, time).await?;
    Ok(())
}


pub async fn write_pid_and_timestamp(
    stream: &mut OwnedWriteHalf,
    pid: &PlayerID,
    time: SystemTime,
) -> Result<(), StreamErr> {
    write_lp_string_owh(stream, &pid.0).await?;
    stream
        .write_u64_le(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
        .await?;
    Ok(())
}

pub fn write_pid_and_timestamp_to_buf(
    buf: &mut Vec<u8>,
    pid: &str,
    time: SystemTime,
) {
    write_lp_string_to_buf(buf, pid);
    buf.extend_from_slice(
        &(time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64)
            .to_le_bytes(),
    );
}

#[derive(Debug)]
pub struct Room {
    // Define room details here
    pub id: u64,
    pub id_str: String,
    pub deets: RoomCreationDeets,
    pub players: RwLock<Vec<Arc<Player>>>,
    pub owner: RwLock<Arc<Player>>,
    pub admins: RwLock<Vec<Arc<Player>>>,
    pub mods: RwLock<Vec<Arc<Player>>>,
    pub actions: RwLock<Vec<Arc<(MapAction, PlayerID, SystemTime, OnceCell<Vec<u8>>)>>>,
    pub has_expired: OnceCell<()>,
    pub player_limit: RwLock<u16>,
    // pub room_rx_action: Receiver<ActionDesc>,
    pub room_tx_action: Sender<ActionDesc>,
    pub action_bytes_sent: RwLock<u64>,
}

impl Room {
    pub async fn start_new_room(player: Player, deets: RoomCreationDeets, action_rx: Receiver<ActionDescArc>) -> Arc<Room> {
        let id = generate_room_id();
        let id_str = room_id_to_str(id);
        let player_arc: Arc<_> = player.into();
        let player_limit = deets.player_limit.into();
        let (room_tx_action, room_rx_action) = channel(52000);
        let room = Room {
            id,
            id_str,
            deets,
            players: vec![player_arc.clone()].into(),
            owner: player_arc.clone().into(),
            admins: vec![player_arc.clone()].into(),
            mods: vec![].into(),
            actions: vec![].into(),
            has_expired: OnceCell::new(),
            player_limit,
            room_tx_action,
            action_bytes_sent: 0.into(),
        };
        let room = Arc::new(room);
        let room_clone = room.clone();
        log::info!(
            "Starting room: {:?} for {:?}",
            room.id_str,
            player_arc.get_pid()
        );
        tokio::spawn(async move {
            room_clone.run(room_rx_action, room_clone.clone()).await;
        });
        let room_clone = room.clone();
        room.add_player_joined_msg(&player_arc).await;
        tokio::spawn(async move { run_player_loop(player_arc, room_clone, action_rx).await });
        log::info!("Room started: {:?}", room.id_str);
        room
    }

    pub async fn add_action(&self, action: ActionDesc) -> Result<(), SendError<ActionDesc>> {
        self.room_tx_action.send(action).await
    }

    /// Server-side effects of relayed actions. Currently: keep the enforced
    /// join limit in sync with Admin_SetRoomPlayerLimit (owner-sent only —
    /// there is no server-side admin tracking yet); it previously stayed at
    /// the room-creation value forever, making the admin action a no-op for
    /// actual join enforcement.
    async fn apply_server_side(&self, action: &ActionDescArc) {
        if let MapAction::Admin_SetRoomPlayerLimit(limit) = &action.0 {
            if (action.1).0 == self.owner.read().await.get_pid() {
                *self.player_limit.write().await = *limit;
                log::info!("[{}] enforced player limit set to {} by owner", self.id_str, limit);
            }
        }
    }

    pub async fn run(&self, mut room_rx_action: Receiver<ActionDesc>, this_room: Arc<Room>) {
        // action reciever loop
        tokio::spawn(async move {
            while !this_room.has_expired.initialized() {
                // recv with a timeout: the Room holds its own Sender, so the
                // channel never closes; a bare recv() would block this task
                // forever after expiry, leaking the Room (and its entire
                // action history) for the life of the server.
                let action1 = match time::timeout(Duration::from_secs(10), room_rx_action.recv()).await {
                    Err(_) => continue, // timeout: re-check has_expired
                    Ok(None) => break,  // channel closed
                    Ok(Some(action)) => action,
                };
                let mut new_actions = vec![Arc::new(action1)];
                // can get error on closed or empty
                while let Ok(action) = room_rx_action.try_recv() {
                    new_actions.push(action.into());
                }
                for a in new_actions.iter() {
                    this_room.apply_server_side(a).await;
                }
                this_room.actions.write().await.extend(new_actions.into_iter());
                // After adding actions, sync players. Stale players are only
                // collected here and removed after the read guard is dropped:
                // player_left takes players.write(), so removing inline while
                // holding players.read() deadlocks and freezes the room.
                let mut stale: Vec<Arc<Player>> = vec![];
                {
                    let players = this_room.players.read().await;
                    let actions = this_room.actions.read().await;
                    for player in players.iter() {
                        if player.action_tx.is_closed() {
                            stale.push(player.clone());
                        } else {
                            player.sync_actions(&actions).await;
                        }
                    }
                }
                for player in stale {
                    log::info!("Player action tx closed, removing player: {:?}", player.get_name());
                    let room = this_room.clone();
                    // in a task: shutdown_err needs stream_w, which a wedged
                    // write to this same dead/slow peer may hold for minutes.
                    tokio::spawn(async move {
                        player.shutdown_err("failed to send actions, please reconnect").await;
                        room.player_left(&player).await;
                    });
                }
            }
        });

        // check if we have no players every 1s. If we have no players for stale_minutes, close the room.
        let mut no_players = 0;
        let wait_ms = 1000;
        // let stale_minutes = 720; // 20; // 720 = 12 hrs
        let stale_minutes = 30; // 20; // 720 = 12 hrs
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
        player: Arc<Player>,
    ) -> Arc<Player> {
        // Add player to room
        log::debug!("[locking] Adding player to room: {:?}", player.get_name());
        // check for previous connections to the player
        let mut rem: Vec<Arc<Player>> = vec![];
        let ps = self.players.write().await;
        for p in ps.iter() {
            if p.get_pid() == player.get_pid() {
                rem.push(p.clone());
            }
        }
        drop(ps);
        // for tests, we reuse the same id so don't disconnect prior connections
        #[cfg(test)]
        {
            rem.drain(0..);
        }
        // remove prior connections if we can
        for p in rem {
            p.shutdown_err("client reconnected").await;
            self.player_left(&p).await;
        }
        // actually join player
        self.add_player_joined_msg(&player).await;
        self.players.write().await.push(player.clone());
        player
    }

    pub async fn add_player_joined_msg(&self, player: &Arc<Player>) {
        log::debug!("player joined, adding join action for {}", player.get_name());
        let _ = self.add_action((
            MapAction::PlayerJoin {
                name: player.get_name().into(),
                account_id: player.get_pid().into(),
            },
            (player.get_pid()).into(),
            SystemTime::now(),
            OnceCell::new(),
        ).into()).await;
        // log::debug!("player joined, unlocked actions");
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
        // 4 + 6 = 10
        let _ = stream.write_u32_le(self.deets.item_max_size).await;
        let _ = stream.write_u16_le(self.deets.player_limit).await;
        // 10 + 4 + 2 = 16
    }

    /// Remove exactly this connection (by Arc identity, not account id: on a
    /// reconnect the old connection's cleanup fires ~1 RTT after the new one
    /// joined, and matching by account id would silently remove the freshly
    /// joined player too). Broadcasts PlayerLeave only if we removed it, so a
    /// second cleanup call (read + write loop both end) is a no-op.
    pub async fn player_left(&self, p: &Arc<Player>) {
        log::debug!("getting players lock");
        let mut players = self.players.write().await;
        log::debug!("got players lock");
        let before = players.len();
        players.retain(|x| !Arc::ptr_eq(x, p));
        let removed = players.len() != before;
        drop(players);
        if !removed {
            return;
        }
        let action = MapAction::PlayerLeave {
            name: p.token_resp.display_name.clone(),
            account_id: p.token_resp.account_id.clone(),
        };
        let _ = self.add_action((
            action,
            (&p.token_resp.account_id).into(),
            SystemTime::now(),
            OnceCell::new(),
        ).into()).await;
    }
}

pub struct RoomManager {
    // Track active rooms
    pub rooms: RwLock<HashMap<u64, Arc<Room>>>, // Example with room ID as key
    pub dump_mbs: bool,
    pub exit_when_idle_mins: Option<u64>,
}

lazy_static! {
    pub static ref TOTAL_PLAYERS: RwLock<u32> = RwLock::new(0);
}

impl RoomManager {
    pub fn new(opts: &ServerOpts) -> Self {
        RoomManager {
            rooms: HashMap::new().into(),
            dump_mbs: opts.dump_macroblocks,
            exit_when_idle_mins: opts.exit_when_idle_mins,
        }
    }

    pub async fn room_mgr_loop(&self) {
        const STATS_LOG_EVERY: Duration = Duration::from_secs(300);
        let mut last_stats_log = std::time::Instant::now() - STATS_LOG_EVERY;
        let mut last_active = std::time::Instant::now();
        loop {
            let rooms = self.rooms.read().await;
            let mut to_remove = vec![];
            let mut total_players: u32 = 0;
            for (id, room) in rooms.iter() {
                if room.has_expired.initialized() {
                    to_remove.push(*id);
                    log::info!("Room expired: {:?}", room.id_str);
                } else {
                    match room.players.try_read() {
                        Ok(ps) => {
                            total_players += ps.len() as u32;
                        },
                        Err(_) => {},
                    }
                }
            }
            let nb_rooms = rooms.len() - to_remove.len();
            drop(rooms);
            *TOTAL_PLAYERS.write().await = total_players;
            if total_players > 0 {
                last_active = std::time::Instant::now();
            }
            if last_stats_log.elapsed() >= STATS_LOG_EVERY {
                last_stats_log = std::time::Instant::now();
                log::info!("[stats] players connected: {} across {} rooms", total_players, nb_rooms);
            }
            if let Some(mins) = self.exit_when_idle_mins {
                if total_players == 0 && last_active.elapsed() >= Duration::from_secs(mins * 60) {
                    log::warn!("[idle-exit] no players for {} min; exiting for supervisor restart (rooms are memory-only and will be lost)", mins);
                    std::process::exit(0);
                }
            }
            if to_remove.len() > 0 {
                let mut rooms = self.rooms.write().await;
                for id in to_remove {
                    rooms.remove(&id);
                }
                drop(rooms);
            }
            time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn manage_room(&self, mut player: Player, action_rx: Receiver<ActionDescArc>) {
        // Room management logic here
        log::debug!(
            "Player connected: {:?} / {:?}",
            player.get_name(),
            player.get_pid()
        );
        let proto_version: u8;
        let mut rx: tokio::sync::RwLockWriteGuard<'_, OwnedReadHalf> = player.stream_r.write().await;
        let mut ver_buf = [0u8; 3];
        // read_exact, not peek: peek returns only the bytes that have already
        // arrived, and the client writes these three bytes separately, so a
        // partial read used to be misreported as a bad version.
        // The timeout stops a stalled/hostile client from parking this
        // connection here forever.
        match time::timeout(Duration::from_secs(20), rx.read_exact(&mut ver_buf)).await {
            Err(_) => {
                log::warn!("Timed out waiting for version bytes");
                player.shutdown_err("timed out waiting for version").await;
                return;
            }
            Ok(Err(e)) => {
                log::error!("Error reading room message: {:?}", e);
                player.shutdown_err("error reading room message").await;
                return;
            }
            Ok(Ok(_)) => {
                // version flag 0xFF and version number with high bit set as flag.
                match client_proto_version(&ver_buf) {
                    Some(v) => proto_version = v,
                    None => {
                        let err_msg = format!("\\$f40Invalid version bytes: 0x {:x} {:x} {:x}, please update", ver_buf[0], ver_buf[1], ver_buf[2]);
                        log::warn!("{}", err_msg);
                        #[cfg(test)]
                        write_lp_string_owh(&mut *player.stream_w.write().await, &err_msg).await.unwrap();
                        // write_lp_string_owh(&mut *player.stream_w.write().await, "\\$s\\$f84 Invalid version: please update.\n").await.unwrap();
                        player.shutdown_err("\\$s\\$f84 Invalid version: please update.\n").await;
                        return;
                    }
                }
            }
        }

        // (version bytes already consumed by read_exact above)
        drop(rx);
        player.proto_version = proto_version;

        // read commands from player.stream
        let room_msg = match time::timeout(Duration::from_secs(30), player.read_room_msg()).await {
            Err(_) => {
                log::warn!("Timed out waiting for room create/join message");
                player.shutdown_err("timed out waiting for room message").await;
                return;
            }
            Ok(r) => r,
        };
        match room_msg {
            Ok(msg) => {
                log::debug!("RoomMsg: {:?}", msg);
                if let Err(e) = player.stream_w.write().await.write_all(b"OK_").await {
                    log::warn!("Error writing OK_ to player: {:?}", e);
                    player.shutdown().await;
                    return;
                }
                match msg {
                    RoomMsg::Create(deets) => {
                        log::debug!("RoomCreationDeets: {:?}", deets);
                        // Create room
                        let room: Arc<Room> = Room::start_new_room(player, deets, action_rx).await;
                        let room_id = room.id_str.clone();
                        log::debug!("Adding room: {:?}", room.id_str);
                        self.rooms.write().await.insert(room.id, room);
                        log::debug!("Added room: {:?}", room_id);
                    }
                    RoomMsg::Join(deets) => {
                        log::debug!("RoomConnectionDeets: {:?}", deets);
                        // Join room
                        let room_id = match str_to_room_id(&deets.room_id) {
                            Some(id) => id,
                            None => {
                                log::warn!("Malformed room id: {:?}", deets.room_id);
                                player.shutdown_err("malformed room id").await;
                                return;
                            }
                        };
                        let rooms = self.rooms.read().await;
                        log::debug!("Got Rooms Read Lock");
                        let room = match rooms.get(&room_id) {
                            Some(room) => room.clone(),
                            None => {
                                log::warn!("Room not found: {}", deets.room_id);
                                player.shutdown_err("room not found").await;
                                return;
                            }
                        };

                        // v5 clients silently mask off the env bits and would open a
                        // Stadium map in a non-Stadium room, desyncing everything.
                        if room.deets.map_env() != 0 && proto_version < 6 {
                            log::warn!(
                                "Refusing v{} client on env-{} room: {}",
                                proto_version, room.deets.map_env(), deets.room_id
                            );
                            player
                                .shutdown_err("\\$s\\$f84 This room uses a custom environment; please update Map Together.\n")
                                .await;
                            return;
                        }

                        let is_xert = player.get_pid() == XERTROV_WSID;
                        let password_match = room.deets.password.len() == 0 || room.deets.password == deets.password;

                        if !password_match && !is_xert {
                            log::warn!("Invalid password for room: {}", deets.room_id);
                            player.shutdown_err("invalid password").await;
                            return;
                        }

                        let is_owner = player.get_pid() == room.owner.read().await.get_pid();

                        // only deny entry by room full if we are not xertrov nor the room owner
                        if !is_owner && !is_xert {
                            if room.players.read().await.len() >= *room.player_limit.read().await as usize {
                                log::warn!("Room full: {}", deets.room_id);
                                player.shutdown_err("Room is full").await;
                                return;
                            }
                        }

                        drop(rooms);
                        log::debug!("Got room: {}", room.id_str.as_str());

                        // player.stream_w.write().await.write(b"OK_").await;
                        let player = Arc::new(player);
                        room.add_player_via_join(player.clone()).await;
                        run_player_loop(player.clone(), room.clone(), action_rx).await;
                    }
                    RoomMsg::Unk(ty, msg) => {
                        log::warn!("Unknown room message type: {} - {}", ty, msg);
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
        // peer_addr fails if the peer already disconnected; don't panic
        let peer = match stream.peer_addr() {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Could not get peer addr (already disconnected?): {:?}", e);
                return;
            }
        };
        log::info!("Initializing connection from: {:?}", peer);

        // Timeout: port scanners connect and send nothing (or a partial
        // length-prefixed read never completes); without this the socket and
        // task are held forever (observed live: weeks-old half-open conns).
        let token = match time::timeout(Duration::from_secs(20), read_lp_string(&mut stream)).await {
            Err(_) => {
                log::warn!("Timed out reading token from {:?}", peer);
                let _ = stream.write_all(b"ERR").await;
                let _ = write_lp_string(&mut stream, "timed out reading token").await;
                let _ = stream.shutdown().await;
                return;
            }
            Ok(t) => t,
        };

        if let Err(e) = token {
            log::error!("Error reading token: {:?}", e);
            let _ = stream.write_all(b"ERR").await;
            let _ = write_lp_string(&mut stream, "malformed token").await;
            let _ = stream.shutdown().await;
            return;
        }

        log::debug!("read token from {:?}", peer);
        let token = token.unwrap();
        if token.len() == 0 {
            log::warn!("Empty token from {:?}", peer);
            let _ = stream.write_all(b"ERR").await;
            let _ = write_lp_string(&mut stream, "empty token").await;
            let _ = stream.shutdown().await;
            return;
        }

        if token.len() < 100 {
            log::warn!("Short token from {:?}", peer);
            let _ = stream.write_all(b"ERR").await;
            let _ = write_lp_string(&mut stream, "auth token too short").await;
            let _ = stream.shutdown().await;
            return;
        }


        log::debug!("Got token of length: {}", token.len());

        let token_resp = match check_token(&token, 521).await {
            Some(token_resp) => token_resp,
            None => {
                log::warn!("Token not verified!");
                let _ = stream.write_all(b"ERR").await;
                let _ = write_lp_string(&mut stream, "auth token verification failed").await;
                let _ = stream.shutdown().await;
                log::warn!("shutdown player, returning");
                return;
            }
        };
        log::info!("TokenResp: {:?}", token_resp);
        let _ = stream.write_all(b"OK_").await;

        let (player, rx) = Player::new(token_resp, stream);

        // Hand off to Room Manager
        self.room_manager.read().await.manage_room(player, rx).await;
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
    check_len_lt_er(len as u16)?;
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
    check_len_lt_er(len as u16)?;
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
    check_len_lt_er(len as u16)?;

    if len > buf.len() - 2 {
        return Err(StreamErr::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("buffer does not contain enough data: {}", len),
        )));
    }
    Ok(String::from_utf8_lossy(&buf[2..(len + 2)]).into_owned())
}

pub fn check_len_lt_er(len: u16) -> Result<(), StreamErr> {
    if len > 10000 {
        return Err(StreamErr::InvalidData(format!("String too long: {}", len)));
    }
    Ok(())
}

pub async fn write_lp_string(stream: &mut TcpStream, s: &str) -> Result<(), StreamErr> {
    let len = s.len() as u16;
    check_len_lt_er(len)?;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(s.as_bytes()).await?;
    Ok(())
}
pub async fn write_lp_string_owh(stream: &mut OwnedWriteHalf, s: &str) -> Result<(), StreamErr> {
    let len = s.len() as u16;
    check_len_lt_er(len)?;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(s.as_bytes()).await?;
    Ok(())
}

pub fn write_lp_string_to_buf(buf: &mut Vec<u8>, s: &str) {
    let len = s.len() as u16;
    check_len_lt_er(len as u16).expect("string lt b'ER'");
    // let buf_pre_len = buf.len();
    // log::debug!("Writing string to buf: {} / {:?}", len, s);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
    // let buf_post_len = buf.len();
    // log::debug!("Wrote string to buf: {:?}", &buf[buf_pre_len..buf_post_len]);
}

#[derive(Debug)]
pub enum StreamErr {
    Io(io::Error),
    Utf8(std::string::FromUtf8Error),
    InvalidData(String),
}

impl Display for StreamErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamErr::Io(e) => write!(f, "IO Error: {}", e),
            StreamErr::Utf8(e) => write!(f, "UTF8 Error: {}", e),
            StreamErr::InvalidData(e) => write!(f, "Invalid Data: {}", e),
        }
    }
}

impl std::error::Error for StreamErr {}

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

    /// Undersized payloads on the raw-indexed message types must surface as
    /// InvalidData, not a panic: a panic here unwinds the read-loop task
    /// before its player_left cleanup, leaving a ghost player in the room.
    #[tokio::test]
    async fn malformed_short_msgs_error_instead_of_panic() {
        use crate::map_actions::{
            MAPPING_MSG_CHAT_MSG, MAPPING_MSG_SERVER_STATS, MAPPING_MSG_SET_ACTION_LIMIT,
            MAPPING_MSG_SET_VARIABLE,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cases: [(u8, Vec<u8>); 5] = [
            (MAPPING_MSG_CHAT_MSG, vec![]),
            (MAPPING_MSG_SET_VARIABLE, vec![0u8; 2]),
            (MAPPING_MSG_SET_VARIABLE, vec![0u8, 0, 0, 0, 5, 0]), // key promises 5 bytes, has 0
            (MAPPING_MSG_SET_ACTION_LIMIT, vec![0u8; 2]),
            (MAPPING_MSG_SERVER_STATS, vec![0u8; 2]),
        ];
        for (ty, payload) in cases {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (server, _) = listener.accept().await.unwrap();
            let token_resp = TokenResp {
                account_id: "test-acct".into(),
                display_name: "tester".into(),
                token_time: 0,
            };
            let (player, _rx) = Player::new(token_resp, server);
            client.write_u8(ty).await.unwrap();
            client.write_u32_le(payload.len() as u32).await.unwrap();
            client.write_all(&payload).await.unwrap();
            client.flush().await.unwrap();
            // slice_to_lp_string reports truncation as StreamErr::Io(InvalidData);
            // either error variant is fine — the point is Err, never a panic.
            let res = player.read_map_msg().await;
            assert!(
                matches!(res, Err(StreamErr::InvalidData(_)) | Err(StreamErr::Io(_))),
                "msg type {} with {}-byte payload: expected an error, got {:?}",
                ty,
                payload.len(),
                res
            );
        }
    }
}
