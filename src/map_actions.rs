use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::managers::*;
use crate::msgs::*;
use crate::mt_codec::*;

/// WSID
#[derive(Debug, Clone)]
pub struct PlayerID(pub String);

impl From<String> for PlayerID {
    fn from(s: String) -> Self {
        PlayerID(s)
    }
}
impl From<&str> for PlayerID {
    fn from(s: &str) -> Self {
        PlayerID(s.to_string())
    }
}
impl From<&String> for PlayerID {
    fn from(s: &String) -> Self {
        PlayerID(s.clone())
    }
}

// should match client and make sure these are used instead of inline numbers
pub const MAPPING_MSG_PLACE: u8 = 1;
pub const MAPPING_MSG_DELETE: u8 = 2;
pub const MAPPING_MSG_RESYNC: u8 = 3;
pub const MAPPING_MSG_SET_SKIN: u8 = 4;
pub const MAPPING_MSG_SET_WAYPOINT: u8 = 5;
pub const MAPPING_MSG_SET_MAPNAME: u8 = 6;
pub const MAPPING_MSG_PLAYER_JOIN: u8 = 7;
pub const MAPPING_MSG_PLAYER_LEAVE: u8 = 8;
pub const MAPPING_MSG_PROMOTE_MOD: u8 = 9;
pub const MAPPING_MSG_DEMOTE_MOD: u8 = 10;
pub const MAPPING_MSG_KICK_PLAYER: u8 = 11;
pub const MAPPING_MSG_BAN_PLAYER: u8 = 12;
pub const MAPPING_MSG_CHANGE_ADMIN: u8 = 13;
pub const MAPPING_MSG_PLAYER_CAMCURSOR: u8 = 14;
pub const MAPPING_MSG_PLAYER_VEHICLEPOS: u8 = 15;
pub const MAPPING_MSG_SET_ACTION_LIMIT: u8 = 16;
pub const MAPPING_MSG_SET_VARIABLE: u8 = 17;
pub const MAPPING_MSG_SET_ROOM_PLAYER_LIMIT: u8 = 18;
pub const MAPPING_MSG_ALERT_STATUS_TO_ALL: u8 = 19;
pub const MAPPING_MSG_CHAT_MSG: u8 = 20;
pub const MAPPING_MSG_PING: u8 = 21;
pub const MAPPING_MSG_SERVER_STATS: u8 = 22;

#[allow(non_camel_case_types)]
#[derive(Debug, Clone)]
pub enum MapAction {
    Place(MacroblockSpec),
    Delete(MacroblockSpec),
    Resync(),
    SetSkin(SetSkinSpec),
    SetWaypoint(WaypointSpec),
    SetMapName(String),
    PlayerJoin {
        name: String,
        account_id: String,
    },
    PlayerLeave {
        name: String,
        account_id: String,
    },
    Admin_PromoteMod(PlayerID),
    Admin_DemoteMod(PlayerID),
    Admin_KickPlayer(PlayerID),
    Admin_BanPlayer(PlayerID),
    Admin_ChangeAdmin(PlayerID),
    PlayerCamCursor(PlayerCamCursor),
    VehiclePos(PlayerVehiclePos),
    Admin_SetActionLimit(u32),
    Admin_SetVariable(String, String),
    Admin_SetRoomPlayerLimit(u16),
    Admin_AlertStatusToAll(String),
    ChatMsg(u8, String),
    /// only sent from player to server
    Ping(),
    /// players connected, response to ping
    ServerStats(u32),
}

impl MapAction {
    pub fn is_ephemeral(&self) -> bool {
        matches!(
            self,
            MapAction::PlayerCamCursor(_) | MapAction::VehiclePos(_)
        )
    }

    pub fn is_ping(&self) -> bool {
        matches!(self, MapAction::Ping())
    }

    pub fn get_type(&self) -> &'static str {
        match self {
            MapAction::Place(_) => "Place",
            MapAction::Delete(_) => "Delete",
            MapAction::Resync() => "Resync",
            MapAction::SetSkin(_) => "SetSkin",
            MapAction::SetWaypoint(_) => "SetWaypoint",
            MapAction::SetMapName(_) => "SetMapName",
            MapAction::PlayerJoin { .. } => "PlayerJoin",
            MapAction::PlayerLeave { .. } => "PlayerLeave",
            MapAction::Admin_PromoteMod(_) => "Admin_PromoteMod",
            MapAction::Admin_DemoteMod(_) => "Admin_DemoteMod",
            MapAction::Admin_KickPlayer(_) => "Admin_KickPlayer",
            MapAction::Admin_BanPlayer(_) => "Admin_BanPlayer",
            MapAction::Admin_ChangeAdmin(_) => "Admin_ChangeAdmin",
            MapAction::PlayerCamCursor(_) => "PlayerCamCursor",
            MapAction::VehiclePos(_) => "VehiclePos",
            MapAction::Admin_SetActionLimit(_) => "Admin_SetActionLimit",
            MapAction::Admin_SetVariable(..) => "Admin_SetVariable",
            MapAction::Admin_SetRoomPlayerLimit(_) => "Admin_SetRoomPlayerLimit",
            MapAction::Admin_AlertStatusToAll(_) => "Admin_AlertStatusToAll",
            MapAction::ChatMsg(_, _) => "ChatMsg",
            MapAction::Ping() => "Ping",
            MapAction::ServerStats(_) => "ServerStats",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MacroblockSpec {
    pub blocks: Vec<BlockSpec>,
    pub items: Vec<ItemSpec>,
    pub terrains: Vec<TerrainSpec>,
}

/// One zone in a terrain cell genealogy column (name + height at that layer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainZone {
    pub name: String,
    pub height: i32,
}

/// One macroblock terrain cell. Layout matches E++ `TerrainSpec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainSpec {
    pub offset: [i32; 3],
    pub zones: Vec<TerrainZone>,
    pub current_index: u32,
    pub dir: u32,
    pub base_height: i32,
    pub bottom_height: i32,
    pub top_height: i32,
}

const MAGIC_BLOCKS: &[u8; 4] = b"BLKs";
const MAGIC_ITEMS: &[u8; 4] = b"ITMs";
const MAGIC_SKINS: &[u8; 4] = b"SKNs";
const MAGIC_TERRAINS: &[u8; 4] = b"TRNs";

fn require_bytes(buf: &[u8], cur: usize, n: usize, what: &str) -> Result<(), StreamErr> {
    if buf.len().saturating_sub(cur) < n {
        return Err(StreamErr::InvalidData(format!("truncated {what}")));
    }
    Ok(())
}

fn read_magic(
    buf: &[u8],
    cur: &mut usize,
    expected: &[u8; 4],
    what: &str,
) -> Result<(), StreamErr> {
    require_bytes(buf, *cur, 4, what)?;
    if &buf[*cur..*cur + 4] != expected {
        return Err(StreamErr::InvalidData(format!(
            "expected magic bytes for {what}"
        )));
    }
    *cur += 4;
    Ok(())
}

fn read_u16_le(buf: &[u8], cur: &mut usize, what: &str) -> Result<u16, StreamErr> {
    require_bytes(buf, *cur, 2, what)?;
    let v = u16::from_le_bytes([buf[*cur], buf[*cur + 1]]);
    *cur += 2;
    Ok(v)
}

fn read_i32_le(buf: &[u8], cur: &mut usize, what: &str) -> Result<i32, StreamErr> {
    require_bytes(buf, *cur, 4, what)?;
    let v = i32::from_le_bytes(buf[*cur..*cur + 4].try_into().unwrap());
    *cur += 4;
    Ok(v)
}

fn read_u32_le(buf: &[u8], cur: &mut usize, what: &str) -> Result<u32, StreamErr> {
    require_bytes(buf, *cur, 4, what)?;
    let v = u32::from_le_bytes(buf[*cur..*cur + 4].try_into().unwrap());
    *cur += 4;
    Ok(v)
}

impl TerrainSpec {
    fn decode_with_len(buf: &[u8]) -> Result<(Self, usize), StreamErr> {
        let mut cur = 0;
        let ox = read_i32_le(buf, &mut cur, "terrain offset")?;
        let oy = read_i32_le(buf, &mut cur, "terrain offset")?;
        let oz = read_i32_le(buf, &mut cur, "terrain offset")?;
        require_bytes(buf, cur, 1, "terrain zone count")?;
        let zone_count = buf[cur];
        cur += 1;
        let mut zones = Vec::with_capacity(zone_count as usize);
        for _ in 0..zone_count {
            let name = slice_to_lp_string(&buf[cur..])?;
            cur += 2 + name.len();
            let height = read_i32_le(buf, &mut cur, "terrain zone height")?;
            zones.push(TerrainZone { name, height });
        }
        let current_index = read_u32_le(buf, &mut cur, "terrain current_index")?;
        let dir = read_u32_le(buf, &mut cur, "terrain dir")?;
        let base_height = read_i32_le(buf, &mut cur, "terrain base_height")?;
        let bottom_height = read_i32_le(buf, &mut cur, "terrain bottom_height")?;
        let top_height = read_i32_le(buf, &mut cur, "terrain top_height")?;
        Ok((
            TerrainSpec {
                offset: [ox, oy, oz],
                zones,
                current_index,
                dir,
                base_height,
                bottom_height,
                top_height,
            },
            cur,
        ))
    }
}

impl MTDecode for TerrainSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        Ok(Self::decode_with_len(buf)?.0)
    }
}

impl MTEncode for TerrainSpec {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.offset[0].to_le_bytes());
        buf.extend_from_slice(&self.offset[1].to_le_bytes());
        buf.extend_from_slice(&self.offset[2].to_le_bytes());
        buf.push(self.zones.len() as u8);
        for zone in &self.zones {
            write_lp_string_to_buf(&mut buf, &zone.name);
            buf.extend_from_slice(&zone.height.to_le_bytes());
        }
        buf.extend_from_slice(&self.current_index.to_le_bytes());
        buf.extend_from_slice(&self.dir.to_le_bytes());
        buf.extend_from_slice(&self.base_height.to_le_bytes());
        buf.extend_from_slice(&self.bottom_height.to_le_bytes());
        buf.extend_from_slice(&self.top_height.to_le_bytes());
        buf
    }
}

impl MTDecode for MacroblockSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        // expect payload to have blocks, skins, and items prefixed with magic bytes in that order
        let mut cur = 0;
        let mut blocks = Vec::new();
        let mut items = Vec::new();

        // blocks
        read_magic(buf, &mut cur, MAGIC_BLOCKS, "blocks")?;
        let block_count = read_u16_le(buf, &mut cur, "block count")?;
        for _ in 0..block_count {
            let block = BlockSpec::decode(&buf[cur..])?;
            cur += block.encoded_len;
            blocks.push(block);
        }

        // skins (expect none)
        read_magic(buf, &mut cur, MAGIC_SKINS, "skins")?;
        let skin_count = read_u16_le(buf, &mut cur, "skin count")?;
        if skin_count > 0 {
            return Err(StreamErr::InvalidData("skins not implemented".to_string()));
        }

        // items
        read_magic(buf, &mut cur, MAGIC_ITEMS, "items")?;
        let item_count = read_u16_le(buf, &mut cur, "item count")?;
        for _ in 0..item_count {
            let item = ItemSpec::decode(&buf[cur..])?;
            cur += item.decoded_len;
            items.push(item);
        }

        // Optional trailing TRNs. Presence of the chunk is the version flag:
        // omit entirely when empty (byte-identical to pre-terrain writers).
        let remaining = buf.len().saturating_sub(cur);
        let terrains = if remaining == 0 {
            Vec::new()
        } else if remaining < 6 {
            return Err(StreamErr::InvalidData(
                "truncated terrain chunk header".to_string(),
            ));
        } else {
            read_magic(buf, &mut cur, MAGIC_TERRAINS, "terrains")?;
            let terrain_count = read_u16_le(buf, &mut cur, "terrain count")?;
            let mut terrains = Vec::with_capacity(terrain_count as usize);
            for _ in 0..terrain_count {
                let (terrain, n) = TerrainSpec::decode_with_len(&buf[cur..])?;
                cur += n;
                terrains.push(terrain);
            }
            if cur != buf.len() {
                return Err(StreamErr::InvalidData(
                    "trailing bytes after terrain chunk".to_string(),
                ));
            }
            terrains
        };

        Ok(MacroblockSpec {
            blocks,
            items,
            terrains,
        })
    }
}

impl MTEncode for MacroblockSpec {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC_BLOCKS);
        buf.extend_from_slice(&(self.blocks.len() as u16).to_le_bytes());
        for block in &self.blocks {
            buf.extend_from_slice(&block.encode());
        }
        buf.extend_from_slice(MAGIC_SKINS);
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(MAGIC_ITEMS);
        buf.extend_from_slice(&(self.items.len() as u16).to_le_bytes());
        for item in &self.items {
            buf.extend_from_slice(&item.encode());
        }
        // Empty TRNs is omitted so old clients see a pre-terrain payload.
        if !self.terrains.is_empty() {
            buf.extend_from_slice(MAGIC_TERRAINS);
            buf.extend_from_slice(&(self.terrains.len() as u16).to_le_bytes());
            for terrain in &self.terrains {
                buf.extend_from_slice(&terrain.encode());
            }
        }
        buf
    }
}

#[derive(Debug, Clone)]
pub struct SetSkinSpec {
    fg_skin: String,
    bg_skin: String,
    block: Option<BlockSpec>,
    item: Option<ItemSpec>,
    enc_size: usize,
}

impl MTDecode for SetSkinSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        let pos = 0;
        let fg_skin = slice_to_lp_string(&buf[pos..])?;
        log::debug!("fg_skin: {:?}", fg_skin);
        let pos = 2 + fg_skin.len();
        let bg_skin = slice_to_lp_string(&buf[pos..])?;
        let pos = pos + 2 + bg_skin.len();
        log::debug!("bg_skin: {:?}", bg_skin);
        log::debug!("next bytes: {:?}", &buf[pos..(pos + 10)]);
        let block = if buf[pos] == 0 {
            log::debug!("block is none");
            None
        } else {
            log::debug!("block is some, {:?}", &buf[pos..pos + 10]);
            Some(BlockSpec::decode(&buf[pos + 1..])?)
        };
        log::debug!("read block, is_some: {:?}", block.is_some());
        let pos = pos + 1 + block.as_ref().map(|b| b.encoded_len).unwrap_or(0);
        let item = if buf[pos] == 0 {
            None
        } else {
            Some(ItemSpec::decode(&buf[pos + 1..])?)
        };
        log::debug!("read item, is_some: {:?}", item.is_some());
        let pos = pos + 1 + item.as_ref().map(|i| i.decoded_len).unwrap_or(0);
        Ok(SetSkinSpec {
            fg_skin,
            bg_skin,
            block,
            item,
            enc_size: pos,
        })
    }
}

impl MTEncode for SetSkinSpec {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.enc_size);
        write_lp_string_to_buf(&mut buf, &self.fg_skin);
        write_lp_string_to_buf(&mut buf, &self.bg_skin);
        if let Some(block) = &self.block {
            buf.push(1);
            buf.extend_from_slice(&block.encode());
        } else {
            buf.push(0);
        }
        if let Some(item) = &self.item {
            buf.push(1);
            buf.extend_from_slice(&item.encode());
        } else {
            buf.push(0);
        }
        buf
    }
}

#[derive(Debug)]
pub struct SkinSpec {
    path: String,
    apply_type: SkinApplyType,
}

impl MTDecode for SkinSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        let path = slice_to_lp_string(&buf[0..])?;
        let apply_type = SkinApplyType::decode(&buf[(2 + path.len())..])?;
        Ok(SkinSpec { path, apply_type })
    }
}

impl MTEncode for SkinSpec {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(self.path.len() as u16).to_le_bytes());
        buf.extend_from_slice(&self.path.as_bytes());
        buf.extend_from_slice(&self.apply_type.encode());
        buf
    }
}

#[derive(Debug)]
pub enum SkinApplyType {
    NormBlock {
        coord: [u32; 3],
        dir: u8,
        ident: String,
    },
    GhostBlock {
        coord: [u32; 3],
        dir: u8,
        ident: String,
    },
    FreeBlock {
        pos: [f32; 3],
        pyr: [f32; 3],
        ident: String,
    },
}

impl MTDecode for SkinApplyType {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        let mut cur = 0;
        let apply_type = match buf[cur] {
            0 => {
                cur += 1;
                let coord = [
                    u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
                    u32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
                    u32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
                ];
                cur += 12;
                let dir = buf[cur];
                cur += 1;
                let ident = slice_to_lp_string(&buf[cur..])?;
                SkinApplyType::NormBlock { coord, dir, ident }
            }
            1 => {
                cur += 1;
                let coord = [
                    u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
                    u32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
                    u32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
                ];
                cur += 12;
                let dir = buf[cur];
                cur += 1;
                let ident = slice_to_lp_string(&buf[cur..])?;
                SkinApplyType::GhostBlock { coord, dir, ident }
            }
            2 => {
                cur += 1;
                let pos = [
                    f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
                    f32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
                    f32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
                ];
                cur += 12;
                let pyr = [
                    f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
                    f32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
                    f32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
                ];
                cur += 12;
                let ident = slice_to_lp_string(&buf[cur..])?;
                SkinApplyType::FreeBlock { pos, pyr, ident }
            }
            _ => {
                return Err(StreamErr::InvalidData(
                    "invalid skin apply type".to_string(),
                ))
            }
        };
        Ok(apply_type)
    }
}

impl MTEncode for SkinApplyType {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            SkinApplyType::NormBlock { coord, dir, ident } => {
                buf.push(0);
                buf.extend_from_slice(&coord[0].to_le_bytes());
                buf.extend_from_slice(&coord[1].to_le_bytes());
                buf.extend_from_slice(&coord[2].to_le_bytes());
                buf.push(*dir);
                buf.extend_from_slice(&(ident.len() as u16).to_le_bytes());
                buf.extend_from_slice(ident.as_bytes());
            }
            SkinApplyType::GhostBlock { coord, dir, ident } => {
                buf.push(1);
                buf.extend_from_slice(&coord[0].to_le_bytes());
                buf.extend_from_slice(&coord[1].to_le_bytes());
                buf.extend_from_slice(&coord[2].to_le_bytes());
                buf.push(*dir);
                buf.extend_from_slice(&(ident.len() as u16).to_le_bytes());
                buf.extend_from_slice(ident.as_bytes());
            }
            SkinApplyType::FreeBlock { pos, pyr, ident } => {
                buf.push(2);
                buf.extend_from_slice(&pos[0].to_le_bytes());
                buf.extend_from_slice(&pos[1].to_le_bytes());
                buf.extend_from_slice(&pos[2].to_le_bytes());
                buf.extend_from_slice(&pyr[0].to_le_bytes());
                buf.extend_from_slice(&pyr[1].to_le_bytes());
                buf.extend_from_slice(&pyr[2].to_le_bytes());
                buf.extend_from_slice(&(ident.len() as u16).to_le_bytes());
                buf.extend_from_slice(ident.as_bytes());
            }
        }
        buf
    }
}

/*
angelscript code for Block and Item specs:

class BlockSpec : NetworkSerializable {
    string name;
    // 26=stadium; 25=stadium256
    uint collection = 26;
    string author;
    nat3 coord;
    CGameCtnBlock::ECardinalDirections dir;
    CGameCtnBlock::ECardinalDirections dir2;
    vec3 pos;
    vec3 pyr;
    CGameCtnBlock::EMapElemColor color;
    CGameCtnBlock::EMapElemLightmapQuality lmQual;
    uint mobilIx;
    uint mobilVariant;
    uint variant;
    uint8 flags;
    WaypointSpec@ waypoint;
}

class WaypointSpec : NetworkSerializable {
    string tag;
    uint order;
}

class ItemSpec : NetworkSerializable {
    string name;
    // 26=stadium; 25=stadium256
    uint collection = 26;
    string author;
    nat3 coord;
    CGameCtnAnchoredObject::ECardinalDirections dir;
    vec3 pos;
    vec3 pyr;
    float scale;
    CGameCtnAnchoredObject::EMapElemColor color;
    CGameCtnAnchoredObject::EMapElemLightmapQuality lmQual;
    CGameCtnAnchoredObject::EPhaseOffset phase;
    mat3 visualRot;
    vec3 pivotPos;
    uint8 isFlying;
    uint16 variantIx;
    uint associatedBlockIx;
    uint itemGroupOnBlock;
    WaypointSpec@ waypoint;
}
*/

#[derive(Debug, Clone)]
pub struct BlockSpec {
    encoded_len: usize,
    name: String,
    collection: u32,
    author: String,
    coord: [u32; 3],
    dir: u8,
    dir2: u8,
    pos: [f32; 3],
    pyr: [f32; 3],
    color: u8,
    lm_qual: u8,
    mobil_ix: u32,
    mobil_variant: u32,
    variant: u32,
    flags: u8,
    waypoint: Option<WaypointSpec>,
}

impl MTDecode for BlockSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        // log::debug!("decoding block spec");
        let mut cur = 0;
        let name = slice_to_lp_string(&buf[cur..])?;
        // log::debug!("block spec name: {:?}", name);
        cur += 2 + name.len();
        let collection = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let author = slice_to_lp_string(&buf[cur..])?;
        // log::debug!("block spec author: {:?}", author);
        cur += 2 + author.len();
        let coord = [
            u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
            u32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
            u32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
        ];
        cur += 12;
        let dir = buf[cur];
        cur += 1;
        let dir2 = buf[cur];
        cur += 1;
        // log::debug!("block spec at pos");
        let pos = [
            f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
            f32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
            f32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
        ];
        cur += 12;
        let pyr = [
            f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
            f32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
            f32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
        ];
        cur += 12;
        let color = buf[cur];
        cur += 1;
        let lm_qual = buf[cur];
        cur += 1;
        let mobil_ix = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let mobil_variant =
            u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let variant = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let flags = buf[cur];
        cur += 1;
        // log::debug!("block spec at waypoint");
        let waypoint = if buf[cur] == 0 {
            None
        } else {
            Some(WaypointSpec::decode(&buf[cur + 1..])?)
        };
        cur += 1 + waypoint.as_ref().map(|w| w.decoded_len).unwrap_or(0);
        let decoded_len = cur;
        Ok(BlockSpec {
            encoded_len: decoded_len,
            name,
            collection,
            author,
            coord,
            dir,
            dir2,
            pos,
            pyr,
            color,
            lm_qual,
            mobil_ix,
            mobil_variant,
            variant,
            flags,
            waypoint,
        })
    }
}

impl MTEncode for BlockSpec {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.encoded_len);
        buf.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        buf.extend_from_slice(self.name.as_bytes());
        buf.extend_from_slice(&self.collection.to_le_bytes());
        buf.extend_from_slice(&(self.author.len() as u16).to_le_bytes());
        buf.extend_from_slice(self.author.as_bytes());
        buf.extend_from_slice(&self.coord[0].to_le_bytes());
        buf.extend_from_slice(&self.coord[1].to_le_bytes());
        buf.extend_from_slice(&self.coord[2].to_le_bytes());
        buf.push(self.dir);
        buf.push(self.dir2);
        buf.extend_from_slice(&self.pos[0].to_le_bytes());
        buf.extend_from_slice(&self.pos[1].to_le_bytes());
        buf.extend_from_slice(&self.pos[2].to_le_bytes());
        buf.extend_from_slice(&self.pyr[0].to_le_bytes());
        buf.extend_from_slice(&self.pyr[1].to_le_bytes());
        buf.extend_from_slice(&self.pyr[2].to_le_bytes());
        buf.push(self.color);
        buf.push(self.lm_qual);
        buf.extend_from_slice(&self.mobil_ix.to_le_bytes());
        buf.extend_from_slice(&self.mobil_variant.to_le_bytes());
        buf.extend_from_slice(&self.variant.to_le_bytes());
        buf.push(self.flags);
        if let Some(waypoint) = &self.waypoint {
            buf.push(1);
            buf.extend_from_slice(&waypoint.encode());
        } else {
            buf.push(0);
        }
        buf
    }
}

#[derive(Debug)]
pub struct SetWaypointSpec {
    waypoint: WaypointSpec,
    block: Option<BlockSpec>,
    item: Option<ItemSpec>,
    enc_size: usize,
}

impl MTDecode for SetWaypointSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        let waypoint = WaypointSpec::decode(buf)?;
        let mut cur = waypoint.decoded_len;
        let block = if buf[cur] == 0 {
            None
        } else {
            Some(BlockSpec::decode(&buf[cur..])?)
        };
        cur += 1 + block.as_ref().map(|b| b.encoded_len).unwrap_or(0);
        let item = if buf[cur] == 0 {
            None
        } else {
            Some(ItemSpec::decode(&buf[cur..])?)
        };
        cur += 1 + item.as_ref().map(|i| i.decoded_len).unwrap_or(0);
        Ok(SetWaypointSpec {
            waypoint,
            block,
            item,
            enc_size: cur,
        })
    }
}

impl MTEncode for SetWaypointSpec {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.enc_size);
        buf.extend_from_slice(&self.waypoint.encode());
        if let Some(block) = &self.block {
            buf.push(1);
            buf.extend_from_slice(&block.encode());
        } else {
            buf.push(0);
        }
        if let Some(item) = &self.item {
            buf.push(1);
            buf.extend_from_slice(&item.encode());
        } else {
            buf.push(0);
        }
        buf
    }
}

#[derive(Debug, Clone)]
pub struct WaypointSpec {
    decoded_len: usize,
    tag: String,
    order: u32,
}

impl MTDecode for WaypointSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        let mut cur = 0;
        let tag = slice_to_lp_string(&buf[cur..])?;
        cur += 2 + tag.len();
        let order = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        Ok(WaypointSpec {
            decoded_len: cur,
            tag,
            order,
        })
    }
}

impl MTEncode for WaypointSpec {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(self.tag.len() as u16).to_le_bytes());
        buf.extend_from_slice(self.tag.as_bytes());
        buf.extend_from_slice(&self.order.to_le_bytes());
        buf
    }
}

#[derive(Debug, Clone)]
pub struct ItemSpec {
    decoded_len: usize,
    name: String,
    collection: u32,
    author: String,
    coord: [u32; 3],
    dir: i8,
    pos: [f32; 3],
    pyr: [f32; 3],
    scale: f32,
    color: u8,
    lm_qual: u8,
    phase: u8,
    visual_rot: [f32; 9],
    pivot_pos: [f32; 3],
    is_flying: u8,
    variant_ix: u16,
    associated_block_ix: u32,
    item_group_on_block: u32,
    waypoint: Option<WaypointSpec>,
}

impl MTDecode for ItemSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        let mut cur = 0;
        let name = slice_to_lp_string(&buf[cur..])?;
        cur += 2 + name.len();
        let collection = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let author = slice_to_lp_string(&buf[cur..])?;
        cur += 2 + author.len();
        let coord = [
            u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
            u32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
            u32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
        ];
        cur += 12;
        let dir = buf[cur] as i8;
        cur += 1;
        let pos = [
            f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
            f32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
            f32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
        ];
        cur += 12;
        let pyr = [
            f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
            f32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
            f32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
        ];
        cur += 12;
        let scale = f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let color = buf[cur];
        cur += 1;
        let lm_qual = buf[cur];
        cur += 1;
        let phase = buf[cur];
        cur += 1;
        let visual_rot = [
            f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
            f32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
            f32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
            f32::from_le_bytes([buf[cur + 12], buf[cur + 13], buf[cur + 14], buf[cur + 15]]),
            f32::from_le_bytes([buf[cur + 16], buf[cur + 17], buf[cur + 18], buf[cur + 19]]),
            f32::from_le_bytes([buf[cur + 20], buf[cur + 21], buf[cur + 22], buf[cur + 23]]),
            f32::from_le_bytes([buf[cur + 24], buf[cur + 25], buf[cur + 26], buf[cur + 27]]),
            f32::from_le_bytes([buf[cur + 28], buf[cur + 29], buf[cur + 30], buf[cur + 31]]),
            f32::from_le_bytes([buf[cur + 32], buf[cur + 33], buf[cur + 34], buf[cur + 35]]),
        ];
        cur += 36;
        let pivot_pos = [
            f32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]),
            f32::from_le_bytes([buf[cur + 4], buf[cur + 5], buf[cur + 6], buf[cur + 7]]),
            f32::from_le_bytes([buf[cur + 8], buf[cur + 9], buf[cur + 10], buf[cur + 11]]),
        ];
        cur += 12;
        let is_flying = buf[cur];
        cur += 1;
        let variant_ix = u16::from_le_bytes([buf[cur], buf[cur + 1]]);
        cur += 2;
        let associated_block_ix =
            u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let item_group_on_block =
            u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let waypoint = if buf[cur] == 0 {
            None
        } else {
            Some(WaypointSpec::decode(&buf[cur + 1..])?)
        };
        cur += 1 + waypoint.as_ref().map(|w| w.decoded_len).unwrap_or(0);
        let decoded_len = cur;
        Ok(ItemSpec {
            decoded_len,
            name,
            collection,
            author,
            coord,
            dir,
            pos,
            pyr,
            scale,
            color,
            lm_qual,
            phase,
            visual_rot,
            pivot_pos,
            is_flying,
            variant_ix,
            associated_block_ix,
            item_group_on_block,
            waypoint,
        })
    }
}

impl MTEncode for ItemSpec {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        buf.extend_from_slice(self.name.as_bytes());
        buf.extend_from_slice(&self.collection.to_le_bytes());
        buf.extend_from_slice(&(self.author.len() as u16).to_le_bytes());
        buf.extend_from_slice(self.author.as_bytes());
        buf.extend_from_slice(&self.coord[0].to_le_bytes());
        buf.extend_from_slice(&self.coord[1].to_le_bytes());
        buf.extend_from_slice(&self.coord[2].to_le_bytes());
        buf.push(self.dir as u8);
        buf.extend_from_slice(&self.pos[0].to_le_bytes());
        buf.extend_from_slice(&self.pos[1].to_le_bytes());
        buf.extend_from_slice(&self.pos[2].to_le_bytes());
        buf.extend_from_slice(&self.pyr[0].to_le_bytes());
        buf.extend_from_slice(&self.pyr[1].to_le_bytes());
        buf.extend_from_slice(&self.pyr[2].to_le_bytes());
        buf.extend_from_slice(&self.scale.to_le_bytes());
        buf.push(self.color);
        buf.push(self.lm_qual);
        buf.push(self.phase);
        buf.extend_from_slice(&self.visual_rot[0].to_le_bytes());
        buf.extend_from_slice(&self.visual_rot[1].to_le_bytes());
        buf.extend_from_slice(&self.visual_rot[2].to_le_bytes());
        buf.extend_from_slice(&self.visual_rot[3].to_le_bytes());
        buf.extend_from_slice(&self.visual_rot[4].to_le_bytes());
        buf.extend_from_slice(&self.visual_rot[5].to_le_bytes());
        buf.extend_from_slice(&self.visual_rot[6].to_le_bytes());
        buf.extend_from_slice(&self.visual_rot[7].to_le_bytes());
        buf.extend_from_slice(&self.visual_rot[8].to_le_bytes());
        buf.extend_from_slice(&self.pivot_pos[0].to_le_bytes());
        buf.extend_from_slice(&self.pivot_pos[1].to_le_bytes());
        buf.extend_from_slice(&self.pivot_pos[2].to_le_bytes());
        buf.push(self.is_flying);
        buf.extend_from_slice(&self.variant_ix.to_le_bytes());
        buf.extend_from_slice(&self.associated_block_ix.to_le_bytes());
        buf.extend_from_slice(&self.item_group_on_block.to_le_bytes());
        if let Some(waypoint) = &self.waypoint {
            buf.push(1);
            buf.extend_from_slice(&waypoint.encode());
        } else {
            buf.push(0);
        }
        buf
    }
}

pub async fn dump_macroblocks(action: MapAction, player: Arc<Player>) {
    match action {
        MapAction::Place(spec) => {
            let now = SystemTime::now();
            let ts = now.duration_since(UNIX_EPOCH).unwrap().as_secs();
            let mut buf = spec.encode();
            write_pid_and_timestamp_to_buf(&mut buf, player.get_pid(), now);
            let fname = format!(
                "./mb-{}-b{}-i{}-t{}.mb_spec",
                ts,
                spec.blocks.len(),
                spec.items.len(),
                spec.terrains.len()
            );
            let _ = tokio::fs::write(&fname, buf).await;
            log::info!("wrote mb spec: {}", fname);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mt_codec::{MTDecode, MTEncode};

    /// `BLKs 0 / SKNs 0 / ITMs 0` — the pre-terrain empty payload.
    fn empty_pre_terrain_payload() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"BLKs");
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(b"SKNs");
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(b"ITMs");
        b.extend_from_slice(&0u16.to_le_bytes());
        b
    }

    /// Same two-zone cell as E++ `MakeTestTerrainSpec`.
    fn sample_terrain() -> TerrainSpec {
        TerrainSpec {
            offset: [3, 0, 7],
            zones: vec![
                TerrainZone {
                    name: "VoidToDirt".into(),
                    height: 0,
                },
                TerrainZone {
                    name: "DirtCliff8".into(),
                    height: 8,
                },
            ],
            current_index: 1,
            dir: 2,
            base_height: 0,
            bottom_height: 0,
            top_height: 8,
        }
    }

    /// Independent of `TerrainSpec::encode`: bytes from the published layout.
    fn sample_terrain_bytes() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&3i32.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&7i32.to_le_bytes());
        b.push(2u8);
        b.extend_from_slice(&10u16.to_le_bytes());
        b.extend_from_slice(b"VoidToDirt");
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&10u16.to_le_bytes());
        b.extend_from_slice(b"DirtCliff8");
        b.extend_from_slice(&8i32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&8i32.to_le_bytes());
        b
    }

    #[test]
    fn old_payload_decodes_empty_terrains_and_reencodes_identically() {
        let input = empty_pre_terrain_payload();
        let spec = MacroblockSpec::decode(&input).expect("decode old payload");
        assert!(spec.blocks.is_empty());
        assert!(spec.items.is_empty());
        assert!(spec.terrains.is_empty());
        assert_eq!(spec.encode(), input);
    }

    #[test]
    fn terrain_roundtrip_preserves_bytes_and_fields() {
        let spec = MacroblockSpec {
            blocks: vec![],
            items: vec![],
            terrains: vec![sample_terrain()],
        };
        let encoded = spec.encode();
        let mut expected = empty_pre_terrain_payload();
        expected.extend_from_slice(b"TRNs");
        expected.extend_from_slice(&1u16.to_le_bytes());
        expected.extend_from_slice(&sample_terrain_bytes());
        assert_eq!(encoded, expected, "encode must match the published layout");

        let decoded = MacroblockSpec::decode(&encoded).expect("decode terrain payload");
        assert_eq!(decoded.terrains, spec.terrains);
        assert_eq!(decoded.encode(), encoded);
    }

    #[test]
    fn trailing_garbage_after_items_is_invalid() {
        let mut input = empty_pre_terrain_payload();
        input.extend_from_slice(b"NOPE");
        input.extend_from_slice(&0u16.to_le_bytes());
        let err = MacroblockSpec::decode(&input).expect_err("garbage after ITMs");
        match err {
            StreamErr::InvalidData(_) => {}
            other => panic!("expected InvalidData, got {other:?}"),
        }
    }

    #[test]
    fn truncated_trns_header_is_invalid_without_panic() {
        let base = empty_pre_terrain_payload();
        for n in 1..=5 {
            let mut input = base.clone();
            input.extend_from_slice(&b"TRNs\x01\x00"[..n]);
            let err = MacroblockSpec::decode(&input)
                .expect_err(&format!("expected Err for {n} leftover bytes"));
            match err {
                StreamErr::InvalidData(_) => {}
                other => panic!("expected InvalidData for {n} leftover bytes, got {other:?}"),
            }
        }
    }

    #[test]
    fn empty_trns_chunk_decodes_empty_and_encode_omits_it() {
        let mut input = empty_pre_terrain_payload();
        input.extend_from_slice(b"TRNs");
        input.extend_from_slice(&0u16.to_le_bytes());
        let spec = MacroblockSpec::decode(&input).expect("decode empty TRNs");
        assert!(spec.terrains.is_empty());
        assert_eq!(spec.encode(), empty_pre_terrain_payload());
    }

    #[test]
    fn truncated_terrain_record_is_invalid_without_panic() {
        let mut input = empty_pre_terrain_payload();
        input.extend_from_slice(b"TRNs");
        input.extend_from_slice(&1u16.to_le_bytes());
        input.extend_from_slice(&3i32.to_le_bytes()); // incomplete record
        let err = MacroblockSpec::decode(&input).expect_err("truncated terrain record");
        match err {
            StreamErr::InvalidData(_) | StreamErr::Io(_) => {}
            other => panic!("expected InvalidData or Io, got {other:?}"),
        }
    }
}
