use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::sync::Arc;

use crate::map_actions::MapAction;
use crate::msgs::{generate_room_id, room_id_to_str, str_to_room_id, RoomConnectionDeets, RoomCreationDeets, RoomMsg};
use crate::{check_token, TokenResp};


#[derive(Debug)]
pub struct Player {
    pub token_resp: TokenResp,
    pub stream: RwLock<TcpStream>,
}

impl Player {
    pub fn new(token_resp: TokenResp, stream: TcpStream) -> Self {
        Player { token_resp, stream: stream.into() }
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
        {
            for p in self.players.read().await.iter() {
                let _ = self.send_player_room_details(p).await;
            }
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
