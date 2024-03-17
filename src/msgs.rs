use serde::{Deserialize, Serialize};

pub const INIT_MSG_ROOM_CREATE: u8 = 1;
pub const INIT_MSG_ROOM_JOIN: u8 = 2;

#[derive(Debug)]
pub enum RoomMsg {
    Unk(u8, String), // Unknown message type
    Create(RoomCreationDeets),
    Join(RoomConnectionDeets),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RoomCreationDeets {
    pub password: String,
    pub action_rate_limit: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RoomConnectionDeets {
    pub room_id: String,
    pub password: String,
}

// 62 ^ 6 = 56,800,235,584

pub fn generate_room_id() -> u64 {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    rng.gen_range(0..56_800_235_584)
}

const ALPHABET: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

pub fn room_id_to_str(id: u64) -> String {
    let mut id = id;
    let mut s = String::new();
    for _ in 0..6 {
        let rem = id % 62;
        id = id / 62;
        s.push(ALPHABET.chars().nth(rem as usize).unwrap());
    }
    s
}

pub fn str_to_room_id(s: &str) -> Option<u64> {
    if s.len() != 6 {
        return None;
    }
    let mut id = 0;
    for (i, c) in s.chars().rev().enumerate() {
        let pos = ALPHABET.find(c).unwrap();
        id += pos as u64 * 62u64.pow(i as u32);
    }
    Some(id)
}
