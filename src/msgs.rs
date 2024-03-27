extern crate rand;

use serde::{Deserialize, Serialize};
use rand::{distributions::{Distribution, Standard}, Rng};

use crate::*;
use crate::mt_codec::*;
use crate::managers::*;

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
    pub map_size: [u8; 3],
    // low bits: mood (0-3), (3 reserved bits), ends with bit flags: NoStadium, Stadium, Stadium155
    pub map_base: u8,
    // base car: 0-3
    pub base_car: u8,
    // allow custom items?, allow delete?, allow selection cut?
    pub rules_flags: u8,
    pub item_max_size: u32,
}

impl RoomCreationDeets {
    pub fn allow_custom_items(&self) -> bool {
        self.rules_flags & 1 == 1
    }
    pub fn allow_delete_all(&self) -> bool {
        self.rules_flags & 2 == 2
    }
    pub fn allow_selection_cut(&self) -> bool {
        self.rules_flags & 4 == 4
    }
}

impl MTEncode for RoomCreationDeets {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_lp_string_to_buf(&mut buf, &self.password);
        buf.extend_from_slice(&self.action_rate_limit.to_le_bytes());
        buf.extend_from_slice(&self.map_size);
        buf.push(self.map_base);
        buf.push(self.base_car);
        buf.push(self.rules_flags);
        buf.extend_from_slice(&self.item_max_size.to_le_bytes());
        buf
    }
}

impl MTDecode for RoomCreationDeets {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        if buf.len() < 15 {
            return Err(StreamErr::InvalidData(format!("RoomCreationDeets: not enough data, expected 15 bytes, got {}", buf.len())));
        }
        let password = slice_to_lp_string(&buf)?;
        let mut idx = 2 + password.len();
        let action_rate_limit = u32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]);
        idx += 4;
        let mut map_size = [0; 3];
        for i in 0..3 {
            map_size[i] = buf[idx + i];
        }
        idx += 3;
        let map_base = buf[idx];
        idx += 1;
        let base_car = buf[idx];
        idx += 1;
        let rules_flags = buf[idx];
        idx += 1;
        let item_max_size = u32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]);
        Ok(Self { password, action_rate_limit, map_size, map_base, base_car, rules_flags, item_max_size })
    }
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
    for (i, c) in s.chars().enumerate() {
        let pos = ALPHABET.find(c).unwrap();
        id += pos as u64 * 62u64.pow(i as u32);
    }
    Some(id)
}







#[derive(Debug, Clone)]
pub struct PlayerVehiclePos {
    pub mat: [[f32; 3]; 4],
    pub vel: [f32; 3],
}

impl Distribution<PlayerVehiclePos> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> PlayerVehiclePos {
        let mut mat = [[0.0; 3]; 4];
        for i in 0..4 {
            for j in 0..3 {
                mat[i][j] = rng.gen();
            }
        }
        let mut vel = [0.0; 3];
        for i in 0..3 {
            vel[i] = rng.gen();
        }
        PlayerVehiclePos { mat, vel }
    }
}

impl MTEncode for PlayerVehiclePos {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        for i in 0..4 {
            for j in 0..3 {
                buf.extend_from_slice(&self.mat[i][j].to_le_bytes());
            }
        }
        for i in 0..3 {
            buf.extend_from_slice(&self.vel[i].to_le_bytes());
        }
        buf
    }
}

impl MTDecode for PlayerVehiclePos {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        if buf.len() < 48 {
            return Err(StreamErr::InvalidData(format!("PlayerVehiclePos: not enough data, expected 48 bytes, got {}", buf.len())));
        }
        let mut mat = [[0.0; 3]; 4];
        let mut idx = 0;
        for i in 0..4 {
            for j in 0..3 {
                mat[i][j] = f32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]);
                idx += 4;
            }
        }
        let mut vel = [0.0; 3];
        for i in 0..3 {
            vel[i] = f32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]);
            idx += 4;
        }
        Ok(Self { mat, vel })
    }
}

#[derive(Debug, Clone)]
pub struct PlayerCamCursor {
    pub cam_matrix: [[f32; 3]; 4],
    pub target: (f32, f32, f32),
    pub cursor: Cursor,
}

impl Distribution<PlayerCamCursor> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> PlayerCamCursor {
        let mut cam_matrix = [[0.0; 3]; 4];
        for i in 0..4 {
            for j in 0..3 {
                cam_matrix[i][j] = rng.gen();
            }
        }
        let target = (rng.gen(), rng.gen(), rng.gen());
        let cursor = rng.gen();
        PlayerCamCursor { cam_matrix, target, cursor }
    }
}

impl MTEncode for PlayerCamCursor {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        for i in 0..4 {
            for j in 0..3 {
                buf.extend_from_slice(&self.cam_matrix[i][j].to_le_bytes());
            }
        }
        buf.extend_from_slice(&self.target.0.to_le_bytes());
        buf.extend_from_slice(&self.target.1.to_le_bytes());
        buf.extend_from_slice(&self.target.2.to_le_bytes());
        buf.extend_from_slice(&self.cursor.encode().as_slice());
        buf
    }
}

impl MTDecode for PlayerCamCursor {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        if buf.len() < 60 {
            return Err(StreamErr::InvalidData(format!("PlayerCamCursor: not enough data, expected 60 bytes, got {}", buf.len())));
        }
        let mut cam_matrix = [[0.0; 3]; 4];
        let mut idx = 0;
        for i in 0..4 {
            for j in 0..3 {
                cam_matrix[i][j] = f32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]);
                idx += 4;
            }
        }
        let target = (
            f32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]),
            f32::from_le_bytes([buf[idx + 4], buf[idx + 5], buf[idx + 6], buf[idx + 7]]),
            f32::from_le_bytes([buf[idx + 8], buf[idx + 9], buf[idx + 10], buf[idx + 11]]),
        );
        idx += 12;
        let cursor = Cursor::decode(&buf[idx..])?;
        Ok(Self { target, cam_matrix, cursor })
    }
}

#[derive(Debug, Clone)]
pub struct Cursor {
    pub edit_mode: u8,
    pub place_mode: u8,
    pub cur_obj: String,
    pub coords: [u32; 3],
    pub pos: [f32; 3],
}


fn generate_rand_string(len: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let one_char = || CHARSET[rng.gen_range(0..CHARSET.len())] as char;
    std::iter::repeat_with(one_char).take(len).collect()
}

impl Distribution<Cursor> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> Cursor {
        let mut coords = [0; 3];
        for i in 0..3 {
            coords[i] = rng.gen();
        }
        let mut pos = [0.0; 3];
        for i in 0..3 {
            pos[i] = rng.gen();
        }
        Cursor {
            edit_mode: rng.gen(),
            place_mode: rng.gen(),
            cur_obj: generate_rand_string(rng.gen_range(8..20)),
            coords,
            pos,
        }
    }
}

impl MTEncode for Cursor {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(self.edit_mode);
        buf.push(self.place_mode);
        write_lp_string_to_buf(&mut buf, &self.cur_obj);
        for i in 0..3 {
            buf.extend_from_slice(&self.coords[i].to_le_bytes());
        }
        for i in 0..3 {
            buf.extend_from_slice(&self.pos[i].to_le_bytes());
        }
        buf
    }
}

impl MTDecode for Cursor {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        if buf.len() < 19 {
            return Err(StreamErr::InvalidData(format!("Cursor: not enough data, expected 19 bytes, got {}", buf.len())));
        }
        let edit_mode = buf[0];
        let place_mode = buf[1];
        let mut idx = 2;
        let cur_obj = slice_to_lp_string(&buf[idx..])?;
        idx += 2 + cur_obj.len();
        let mut coords = [0; 3];
        for i in 0..3 {
            coords[i] = u32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]);
            idx += 4;
        }
        let mut pos = [0.0; 3];
        for i in 0..3 {
            pos[i] = f32::from_le_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]);
            idx += 4;
        }
        Ok(Self { edit_mode, place_mode, cur_obj, coords, pos })
    }
}
