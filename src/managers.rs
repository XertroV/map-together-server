use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::sync::Arc;
use std::time::SystemTime;

use crate::map_actions::{MacroblockSpec, MapAction, SkinSpec, WaypointSpec};
use crate::msgs::{generate_room_id, room_id_to_str, str_to_room_id, RoomConnectionDeets, RoomCreationDeets, RoomMsg};
use crate::mt_codec::{MTDecode, MTEncode};
use crate::{check_token, TokenResp};


#[derive(Debug, Default)]
struct PlayerSync {
    last_sync: usize,
}

#[derive(Debug)]
pub struct Player {
    pub token_resp: TokenResp,
    pub stream: RwLock<TcpStream>,
    sync: RwLock<PlayerSync>,
}


impl Player {
    pub fn new(token_resp: TokenResp, stream: TcpStream) -> Self {
        Player { token_resp, stream: stream.into(), sync: RwLock::new(PlayerSync::default()) }
    }

    pub async fn sync_actions(&self, actions: &Vec<MapAction>) {
        let mut sync = self.sync.write().await;
        let new_actions = &actions[sync.last_sync..];
        for action in new_actions {
            self.write_action(action).await;
        }
        sync.last_sync = actions.len();
    }

    pub async fn write_action(&self, action: &MapAction) {
        let mut stream = self.stream.write().await;
        match action {
            MapAction::Place(mb) => {
                let buf = mb.encode();
                stream.write_u8(1).await.unwrap();
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
            }
            MapAction::Delete(mb) => {
                let buf = mb.encode();
                stream.write_u8(2).await.unwrap();
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
            }
            MapAction::Resync() => {
                stream.write_u8(3).await.unwrap();
            }
            MapAction::SetSkin(skin) => {
                let buf = skin.encode();
                stream.write_u8(4).await.unwrap();
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
            }
            MapAction::SetWaypoint(wp) => {
                let buf = wp.encode();
                stream.write_u8(5).await.unwrap();
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
            }
            MapAction::SetMapName(name) => {
                stream.write_u8(6).await.unwrap();
                write_lp_string(&mut stream, name).await.unwrap();
            }
            MapAction::PlayerJoin { name, login } => {
                let mut buf = vec![];
                buf.extend_from_slice(&name.len().to_le_bytes());
                buf.extend_from_slice(name.as_bytes());
                buf.extend_from_slice(&login.len().to_le_bytes());
                buf.extend_from_slice(login.as_bytes());
                stream.write_u8(7).await.unwrap();
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
            }
            MapAction::PlayerLeave { name, login } => {
                let mut buf = vec![];
                buf.extend_from_slice(&name.len().to_le_bytes());
                buf.extend_from_slice(name.as_bytes());
                buf.extend_from_slice(&login.len().to_le_bytes());
                buf.extend_from_slice(login.as_bytes());
                stream.write_u8(8).await.unwrap();
                stream.write_u32_le(buf.len() as u32).await.unwrap();
                stream.write_all(&buf).await.unwrap();
            }
        }
    }


    pub async fn read_room_msg(&mut self) -> Result<RoomMsg, StreamErr> {
        let mut _stream = self.stream.write().await;
        let mut stream = _stream.deref_mut();
        let msg_ty = stream.read_u8().await?;
        match msg_ty {
            1 => {
                // Room creation
                let password = read_lp_string(&mut stream).await?;
                let action_rate_limit = stream.read_u32().await?;
                // Ok(format!("Room creation: password: {}, action_rate_limit: {}", password, action_rate_limit))
                Ok(RoomMsg::Create(RoomCreationDeets { password, action_rate_limit }))
            }
            2 => {
                // Room join
                let room_id = read_lp_string(&mut stream).await?;
                let password = read_lp_string(&mut stream).await?;
                // Ok(format!("Room join: room_id: {}, password: {}", room_id, password))
                Ok(RoomMsg::Join(RoomConnectionDeets { room_id, password }))
            }
            _ => {
                Ok(RoomMsg::Unk(msg_ty, format!("Unknown message type")))
            }
        }
    }

    pub async fn read_map_msg(&self) -> Result<MapAction, StreamErr> {
        let mut _stream = self.stream.write().await;
        let stream = _stream.deref_mut();
        let msg_ty = stream.read_u8().await?;
        // expect to read MapAction via MAPPING_MSG_* constants
        match msg_ty {
            1 => {
                // Place
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let mb = MacroblockSpec::decode(&buf)?;
                Ok(MapAction::Place(mb))
            },
            2 => {
                // Delete
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let mb = MacroblockSpec::decode(&buf)?;
                Ok(MapAction::Delete(mb))
            },
            3 => {
                // resync
                Ok(MapAction::Resync())
            },
            4 => {
                // set skin
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::SetSkin(SkinSpec::decode(&buf)?))
            },
            5 => {
                // set waypoint
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                Ok(MapAction::SetWaypoint(WaypointSpec::decode(&buf)?))
            },
            6 => {
                // set map name
                Ok(MapAction::SetMapName(read_lp_string(stream).await?))
            }
            7 => {
                // player join
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let name = slice_to_lp_string(&buf[4..])?;
                let login = slice_to_lp_string(&buf[4 + 2 + name.len()..])?;
                Ok(MapAction::PlayerJoin { name, login })
            }
            8 => {
                // player leave
                let len = stream.read_u32_le().await?;
                let mut buf = vec![0u8; len as usize];
                stream.read_exact(&mut buf).await?;
                let name = slice_to_lp_string(&buf[4..])?;
                let login = slice_to_lp_string(&buf[4 + 2 + name.len()..])?;
                Ok(MapAction::PlayerLeave { name, login })
            }
            _ => {
                Err(StreamErr::InvalidData(format!("Unknown message type: {}", msg_ty)))
            }
        }

    }

    pub async fn shutdown(&self) {
        let _ = self.stream.write().await.shutdown().await;
    }

    pub async fn shutdown_err(&self, arg: &str) {
        let mut stream = self.stream.write().await;
        let _ = stream.write_all(b"ERR").await;
        let _ = write_lp_string(&mut stream, arg).await;
        drop(stream);
        self.shutdown().await;
    }
}


#[derive(Debug)]
pub struct Room {
    // Define room details here
    pub id: u64,
    pub id_str: String,
    pub password: String,
    pub action_rate_limit: u32,
    pub players: RwLock<Vec<Arc<Player>>>,
    pub owner: RwLock<Arc<Player>>,
    pub mods: RwLock<Vec<Arc<Player>>>,
    pub actions: RwLock<Vec<MapAction>>,
}



impl Room {
    pub async fn start_new_room(player: Player, deets: RoomCreationDeets) -> Arc<Room> {
        let id = generate_room_id();
        let id_str = room_id_to_str(id);
        let player_arc: Arc<_> = player.into();
        let room = Room {
            id,
            id_str,
            password: deets.password,
            action_rate_limit: deets.action_rate_limit,
            players: vec![player_arc.clone()].into(),
            owner: player_arc.clone().into(),
            mods: vec![].into(),
            actions: vec![].into(),
        };
        let room = Arc::new(room);
        let room_clone = room.clone();
        tokio::spawn(async move {
            room_clone.run().await;
        });
        room
    }

    pub async fn run(&self) {
        // init any players we already have (probs just admin -- we will assume so atm)
        {
            for p in self.players.read().await.iter() {
                let _ = self.send_player_room_details(p).await;
            }
        }
        let loop_max_hz = 100.0;
        let loop_max_ms = 1000.0 / loop_max_hz;
        let mut carry = 0.0;
        // main loop
        loop {
            let start = SystemTime::now();
            // read actions from players
            let mut new_actions: Vec<MapAction> = vec![];
            let mut buf = [0u8; 4];
            for p in self.players.read().await.iter() {
                // check if we can read a message
                if let Ok(_) = p.stream.read().await.peek(&mut buf).await {
                    match p.read_map_msg().await {
                        Ok(action) => {
                            new_actions.push(action);
                        }
                        Err(e) => {
                            log::error!("Error reading map message: {:?}", e);
                            p.shutdown_err("error reading map message").await;
                        }
                    }
                }
            }

            let mut actions = self.actions.write().await;
            actions.append(&mut new_actions);
            drop(actions);

            let actions = self.actions.read().await;
            for p in self.players.read().await.iter() {
                p.sync_actions(&actions).await;
            }

            let elapsed = start.elapsed().unwrap();
            let elapsed_ms = elapsed.as_millis() as f64 + carry;
            if elapsed_ms < loop_max_ms {
                tokio::time::sleep(tokio::time::Duration::from_millis((loop_max_ms - elapsed_ms).round() as u64)).await;
            }
            carry = (start.elapsed().unwrap().as_secs_f64() * 1000.0 - loop_max_ms).max(0.0);
        }
    }

    pub async fn add_player_via_join(&self, deets: RoomConnectionDeets, player: Player) {
        // Add player to room
        if self.password != deets.password {
            log::warn!("Invalid password for room: {}", deets.room_id);
            player.shutdown_err("invalid password").await;
            return;
        }
        self.send_player_room_details(&player).await;
        self.players.write().await.push(player.into());
    }

    pub async fn send_player_room_details(&self, player: &Player) {
        let mut stream = player.stream.write().await;
        let _ = write_lp_string(&mut stream, &self.id_str).await;
        let _ = stream.write_u32(self.action_rate_limit).await;
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

    pub async fn manage_room(&mut self, mut player: Player) {
        // Room management logic here
        log::debug!("Player connected: {:?}", player);
        // read commands from player.stream
        match player.read_room_msg().await {
            Ok(msg) => {
                log::debug!("RoomMsg: {:?}", msg);
                match msg {
                    RoomMsg::Create(deets) => {
                        log::debug!("RoomCreationDeets: {:?}", deets);
                        // Create room
                        let room: Arc<Room> = Room::start_new_room(player, deets).await;
                        self.rooms.write().await.insert(room.id, room);
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
                        room.add_player_via_join(deets, player).await;
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
        log::info!("Initializing connection: {:?}", stream);

        let token = read_lp_string(&mut stream).await;
        if let Err(e) = token {
            log::error!("Error reading token: {:?}", e);
            let _ = stream.write_all(b"ERR").await;
            let _ = write_lp_string(&mut stream, "malformed token").await;
            stream.shutdown().await.unwrap();
            return;
        }

        let token = token.unwrap();
        let token_resp = match check_token(&token, 521).await {
            Some(token_resp) => token_resp,
            None => {
                log::warn!("Token not found!");
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
        let mut rm = self.room_manager.write().await;
        rm.manage_room(player).await;
    }
}


pub async fn read_lp_string(stream: &mut TcpStream) -> Result<String, StreamErr> {
    let mut buf = [0u8; 2];
    stream.read_exact(&mut buf).await?;
    let len = u16::from_le_bytes(buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(String::from_utf8(buf.to_vec())?)
}

pub fn slice_to_lp_string(buf: &[u8]) -> Result<String, StreamErr> {
    if buf.len() < 2 {
        return Err(StreamErr::Io(io::Error::new(io::ErrorKind::InvalidData, "buffer too small")));
    }
    let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    Ok(String::from_utf8_lossy(&buf[2..(len + 2)]).into_owned())
}

pub async fn write_lp_string(stream: &mut TcpStream, s: &str) -> Result<(), StreamErr> {
    let len = s.len() as u16;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(s.as_bytes()).await?;
    Ok(())
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
