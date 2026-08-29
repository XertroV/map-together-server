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
pub const MAPPING_MSG_PLAYER_TEST_MODE: u8 = 23;

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
    /// test-mode enter/leave + car-skin url (ephemeral; v6+ only)
    PlayerTestMode(PlayerTestMode),
}

impl MapAction {
    pub fn is_ephemeral(&self) -> bool {
        matches!(
            self,
            MapAction::PlayerCamCursor(_)
                | MapAction::VehiclePos(_)
                | MapAction::PlayerTestMode(_)
        )
    }

    pub fn is_ping(&self) -> bool {
        matches!(self, MapAction::Ping())
    }

    /// Minimum client protocol version that understands this message type.
    /// Older clients crash on unknown message types, so the write loop drops
    /// any action its recipient is too old for (see `run_player_loop`).
    pub fn min_proto_version(&self) -> u8 {
        match self {
            MapAction::PlayerTestMode(_) => 6,
            _ => 0,
        }
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
            MapAction::PlayerTestMode(_) => "PlayerTestMode",
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

fn decode_flagged<T: MTDecode>(
    cur: &mut DecodeCursor,
    len_of: impl Fn(&T) -> usize,
    what: &str,
) -> Result<Option<T>, StreamErr> {
    let flag = cur.u8(what)?;
    if flag == 0 {
        Ok(None)
    } else {
        let v = T::decode(cur.rest())?;
        cur.skip(len_of(&v), what)?;
        Ok(Some(v))
    }
}

impl TerrainSpec {
    fn decode_from(cur: &mut DecodeCursor) -> Result<Self, StreamErr> {
        let ox = cur.i32_le("terrain offset")?;
        let oy = cur.i32_le("terrain offset")?;
        let oz = cur.i32_le("terrain offset")?;
        let zone_count = cur.u8("terrain zone count")?;
        let mut zones = Vec::with_capacity(zone_count as usize);
        for _ in 0..zone_count {
            let name = cur.lp_string("terrain zone name")?;
            let height = cur.i32_le("terrain zone height")?;
            zones.push(TerrainZone { name, height });
        }
        Ok(TerrainSpec {
            offset: [ox, oy, oz],
            zones,
            current_index: cur.u32_le("terrain current_index")?,
            dir: cur.u32_le("terrain dir")?,
            base_height: cur.i32_le("terrain base_height")?,
            bottom_height: cur.i32_le("terrain bottom_height")?,
            top_height: cur.i32_le("terrain top_height")?,
        })
    }
}

impl MTDecode for TerrainSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        Self::decode_from(&mut DecodeCursor::new(buf))
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
        let mut cur = DecodeCursor::new(buf);
        let mut blocks = Vec::new();
        let mut items = Vec::new();

        cur.magic(MAGIC_BLOCKS, "blocks")?;
        let block_count = cur.u16_le("block count")?;
        for _ in 0..block_count {
            let block = BlockSpec::decode(cur.rest())?;
            cur.skip(block.encoded_len, "block")?;
            blocks.push(block);
        }

        cur.magic(MAGIC_SKINS, "skins")?;
        let skin_count = cur.u16_le("skin count")?;
        if skin_count > 0 {
            return Err(StreamErr::InvalidData("skins not implemented".to_string()));
        }

        cur.magic(MAGIC_ITEMS, "items")?;
        let item_count = cur.u16_le("item count")?;
        for _ in 0..item_count {
            let item = ItemSpec::decode(cur.rest())?;
            cur.skip(item.decoded_len, "item")?;
            items.push(item);
        }

        // Optional trailing TRNs. Presence of the chunk is the version flag:
        // omit entirely when empty (byte-identical to pre-terrain writers).
        let terrains = if cur.is_empty() {
            Vec::new()
        } else if cur.remaining() < 6 {
            return Err(StreamErr::InvalidData(
                "truncated terrain chunk header".to_string(),
            ));
        } else {
            cur.magic(MAGIC_TERRAINS, "terrains")?;
            let terrain_count = cur.u16_le("terrain count")?;
            let mut terrains = Vec::with_capacity(terrain_count as usize);
            for _ in 0..terrain_count {
                terrains.push(TerrainSpec::decode_from(&mut cur)?);
            }
            if !cur.is_empty() {
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
        let mut cur = DecodeCursor::new(buf);
        let fg_skin = cur.lp_string("fg_skin")?;
        log::debug!("fg_skin: {:?}", fg_skin);
        let bg_skin = cur.lp_string("bg_skin")?;
        log::debug!("bg_skin: {:?}", bg_skin);
        let block = decode_flagged(&mut cur, |b: &BlockSpec| b.encoded_len, "skin block")?;
        log::debug!("read block, is_some: {:?}", block.is_some());
        let item = decode_flagged(&mut cur, |i: &ItemSpec| i.decoded_len, "skin item")?;
        log::debug!("read item, is_some: {:?}", item.is_some());
        Ok(SetSkinSpec {
            fg_skin,
            bg_skin,
            block,
            item,
            enc_size: cur.pos(),
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
        let mut cur = DecodeCursor::new(buf);
        let path = cur.lp_string("skin path")?;
        let apply_type = SkinApplyType::decode(cur.rest())?;
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
        let mut cur = DecodeCursor::new(buf);
        match cur.u8("skin apply type")? {
            0 => {
                let coord = cur.u32x3("skin coord")?;
                let dir = cur.u8("skin dir")?;
                let ident = cur.lp_string("skin ident")?;
                Ok(SkinApplyType::NormBlock { coord, dir, ident })
            }
            1 => {
                let coord = cur.u32x3("skin coord")?;
                let dir = cur.u8("skin dir")?;
                let ident = cur.lp_string("skin ident")?;
                Ok(SkinApplyType::GhostBlock { coord, dir, ident })
            }
            2 => {
                let pos = cur.f32x3("skin pos")?;
                let pyr = cur.f32x3("skin pyr")?;
                let ident = cur.lp_string("skin ident")?;
                Ok(SkinApplyType::FreeBlock { pos, pyr, ident })
            }
            _ => Err(StreamErr::InvalidData(
                "invalid skin apply type".to_string(),
            )),
        }
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
        let mut cur = DecodeCursor::new(buf);
        let name = cur.lp_string("block name")?;
        let collection = cur.u32_le("block collection")?;
        let author = cur.lp_string("block author")?;
        let coord = cur.u32x3("block coord")?;
        let dir = cur.u8("block dir")?;
        let dir2 = cur.u8("block dir2")?;
        let pos = cur.f32x3("block pos")?;
        let pyr = cur.f32x3("block pyr")?;
        let color = cur.u8("block color")?;
        let lm_qual = cur.u8("block lm_qual")?;
        let mobil_ix = cur.u32_le("block mobil_ix")?;
        let mobil_variant = cur.u32_le("block mobil_variant")?;
        let variant = cur.u32_le("block variant")?;
        let flags = cur.u8("block flags")?;
        let waypoint =
            decode_flagged(&mut cur, |w: &WaypointSpec| w.decoded_len, "block waypoint")?;
        Ok(BlockSpec {
            encoded_len: cur.pos(),
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
        let mut cur = DecodeCursor::new(buf);
        let waypoint = WaypointSpec::decode(cur.rest())?;
        cur.skip(waypoint.decoded_len, "waypoint")?;
        // Preserve existing layout: a non-zero flag is the first byte of the
        // nested spec (not skipped before decode). Dead path today.
        cur.require(1, "waypoint block flag")?;
        let block = if cur.rest()[0] == 0 {
            cur.u8("waypoint block flag")?;
            None
        } else {
            let block = BlockSpec::decode(cur.rest())?;
            cur.skip(1 + block.encoded_len, "waypoint block")?;
            Some(block)
        };
        cur.require(1, "waypoint item flag")?;
        let item = if cur.rest()[0] == 0 {
            cur.u8("waypoint item flag")?;
            None
        } else {
            let item = ItemSpec::decode(cur.rest())?;
            cur.skip(1 + item.decoded_len, "waypoint item")?;
            Some(item)
        };
        Ok(SetWaypointSpec {
            waypoint,
            block,
            item,
            enc_size: cur.pos(),
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
        let mut cur = DecodeCursor::new(buf);
        let tag = cur.lp_string("waypoint tag")?;
        let order = cur.u32_le("waypoint order")?;
        Ok(WaypointSpec {
            decoded_len: cur.pos(),
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
        let mut cur = DecodeCursor::new(buf);
        let name = cur.lp_string("item name")?;
        let collection = cur.u32_le("item collection")?;
        let author = cur.lp_string("item author")?;
        let coord = cur.u32x3("item coord")?;
        let dir = cur.i8("item dir")?;
        let pos = cur.f32x3("item pos")?;
        let pyr = cur.f32x3("item pyr")?;
        let scale = cur.f32_le("item scale")?;
        let color = cur.u8("item color")?;
        let lm_qual = cur.u8("item lm_qual")?;
        let phase = cur.u8("item phase")?;
        let visual_rot = cur.f32x9("item visual_rot")?;
        let pivot_pos = cur.f32x3("item pivot_pos")?;
        let is_flying = cur.u8("item is_flying")?;
        let variant_ix = cur.u16_le("item variant_ix")?;
        let associated_block_ix = cur.u32_le("item associated_block_ix")?;
        let item_group_on_block = cur.u32_le("item item_group_on_block")?;
        let waypoint = decode_flagged(&mut cur, |w: &WaypointSpec| w.decoded_len, "item waypoint")?;
        Ok(ItemSpec {
            decoded_len: cur.pos(),
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

    fn assert_invalid(buf: &[u8], why: &str) {
        match MacroblockSpec::decode(buf) {
            Err(StreamErr::InvalidData(_)) | Err(StreamErr::Io(_)) => {}
            Err(other) => panic!("{why}: expected InvalidData or Io, got {other:?}"),
            Ok(_) => panic!("{why}: expected Err"),
        }
    }

    #[test]
    fn leftover_after_valid_trns_is_invalid() {
        let spec = MacroblockSpec {
            blocks: vec![],
            items: vec![],
            terrains: vec![sample_terrain()],
        };
        let mut buf = spec.encode();
        buf.push(0xFF);
        assert_invalid(&buf, "byte after a complete TRNs chunk");
    }

    #[test]
    fn leftover_after_empty_trns_is_invalid() {
        let mut input = empty_pre_terrain_payload();
        input.extend_from_slice(b"TRNs");
        input.extend_from_slice(&0u16.to_le_bytes());
        input.push(0x00);
        assert_invalid(&input, "byte after empty TRNs");
    }

    #[test]
    fn two_terrain_cells_roundtrip() {
        let mut second = sample_terrain();
        second.offset = [4, -1, 8];
        second.dir = 3;
        second.base_height = -8;
        second.bottom_height = -8;
        second.top_height = 16;
        let spec = MacroblockSpec {
            blocks: vec![],
            items: vec![],
            terrains: vec![sample_terrain(), second.clone()],
        };
        let encoded = spec.encode();
        let decoded = MacroblockSpec::decode(&encoded).expect("two cells");
        assert_eq!(decoded.terrains.len(), 2);
        assert_eq!(decoded.terrains[0], sample_terrain());
        assert_eq!(decoded.terrains[1], second);
        assert_eq!(decoded.encode(), encoded);
    }

    #[test]
    fn zero_zone_terrain_roundtrip() {
        let cell = TerrainSpec {
            offset: [1, 2, 3],
            zones: vec![],
            current_index: 0,
            dir: 0,
            base_height: -4,
            bottom_height: -4,
            top_height: 0,
        };
        let spec = MacroblockSpec {
            blocks: vec![],
            items: vec![],
            terrains: vec![cell.clone()],
        };
        let encoded = spec.encode();
        // Independent layout: offset 12 + zone_count 1 + trailer 20 = 33, plus TRNs/u16.
        let mut expected = empty_pre_terrain_payload();
        expected.extend_from_slice(b"TRNs");
        expected.extend_from_slice(&1u16.to_le_bytes());
        expected.extend_from_slice(&1i32.to_le_bytes());
        expected.extend_from_slice(&2i32.to_le_bytes());
        expected.extend_from_slice(&3i32.to_le_bytes());
        expected.push(0u8);
        expected.extend_from_slice(&0u32.to_le_bytes());
        expected.extend_from_slice(&0u32.to_le_bytes());
        expected.extend_from_slice(&(-4i32).to_le_bytes());
        expected.extend_from_slice(&(-4i32).to_le_bytes());
        expected.extend_from_slice(&0i32.to_le_bytes());
        assert_eq!(encoded, expected);
        let decoded = MacroblockSpec::decode(&encoded).expect("zero-zone cell");
        assert_eq!(decoded.terrains, vec![cell]);
    }

    #[test]
    fn short_or_empty_buffer_is_invalid_without_panic() {
        assert_invalid(&[], "empty buffer");
        assert_invalid(&[b'B', b'L', b'K'], "truncated BLKs magic");
        let mut almost = empty_pre_terrain_payload();
        almost.truncate(almost.len() - 1);
        assert_invalid(&almost, "truncated item count");
    }

    #[test]
    fn skins_count_nonzero_is_invalid() {
        let mut input = Vec::new();
        input.extend_from_slice(b"BLKs");
        input.extend_from_slice(&0u16.to_le_bytes());
        input.extend_from_slice(b"SKNs");
        input.extend_from_slice(&1u16.to_le_bytes());
        input.extend_from_slice(b"ITMs");
        input.extend_from_slice(&0u16.to_le_bytes());
        let err = MacroblockSpec::decode(&input).expect_err("skins");
        match err {
            StreamErr::InvalidData(msg) => assert!(msg.contains("skins")),
            other => panic!("expected InvalidData, got {other:?}"),
        }
    }

    #[test]
    fn truncated_zone_name_is_invalid_without_panic() {
        let mut input = empty_pre_terrain_payload();
        input.extend_from_slice(b"TRNs");
        input.extend_from_slice(&1u16.to_le_bytes());
        input.extend_from_slice(&0i32.to_le_bytes());
        input.extend_from_slice(&0i32.to_le_bytes());
        input.extend_from_slice(&0i32.to_le_bytes());
        input.push(1u8); // one zone
        input.extend_from_slice(&10u16.to_le_bytes());
        input.extend_from_slice(b"Voi"); // 3 of 10 name bytes
        assert_invalid(&input, "truncated zone name");
    }

    #[test]
    fn wrong_leading_magic_is_invalid() {
        let mut input = empty_pre_terrain_payload();
        input[0..4].copy_from_slice(b"NOPE");
        assert_invalid(&input, "wrong BLKs magic");
    }

    #[test]
    fn nested_specs_short_buffer_is_invalid_without_panic() {
        match BlockSpec::decode(&[]) {
            Err(StreamErr::InvalidData(_)) | Err(StreamErr::Io(_)) => {}
            other => panic!("BlockSpec: {other:?}"),
        }
        match ItemSpec::decode(&[1, 0, b'x']) {
            Err(StreamErr::InvalidData(_)) | Err(StreamErr::Io(_)) => {}
            other => panic!("ItemSpec: {other:?}"),
        }
        match WaypointSpec::decode(&[]) {
            Err(StreamErr::InvalidData(_)) | Err(StreamErr::Io(_)) => {}
            other => panic!("WaypointSpec: {other:?}"),
        }
    }

    /// Hand-built empty-ish block: name "A", collection 26, empty author, zeros, no waypoint.
    fn minimal_block_bytes() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&1u16.to_le_bytes());
        b.push(b'A');
        b.extend_from_slice(&26u32.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&[0u8; 12]); // coord
        b.push(0);
        b.push(0);
        b.extend_from_slice(&[0u8; 12]); // pos
        b.extend_from_slice(&[0u8; 12]); // pyr
        b.push(0);
        b.push(0);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.push(0); // flags
        b.push(0); // no waypoint
        b
    }

    #[test]
    fn block_plus_terrain_roundtrip() {
        let block = BlockSpec::decode(&minimal_block_bytes()).expect("minimal block");
        let spec = MacroblockSpec {
            blocks: vec![block],
            items: vec![],
            terrains: vec![sample_terrain()],
        };
        let encoded = spec.encode();
        let decoded = MacroblockSpec::decode(&encoded).expect("block+terrain");
        assert_eq!(decoded.blocks.len(), 1);
        assert!(decoded.items.is_empty());
        assert_eq!(decoded.terrains, vec![sample_terrain()]);
        assert_eq!(decoded.encode(), encoded);
        assert!(
            encoded.windows(4).any(|w| w == b"TRNs"),
            "mixed payload must carry TRNs"
        );
    }

    #[test]
    fn waypoint_roundtrip() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&2u16.to_le_bytes());
        raw.extend_from_slice(b"cp");
        raw.extend_from_slice(&3u32.to_le_bytes());
        let wp = WaypointSpec::decode(&raw).expect("waypoint");
        assert_eq!(wp.tag, "cp");
        assert_eq!(wp.order, 3);
        assert_eq!(wp.encode(), raw);
    }

    #[test]
    fn player_test_mode_action_flags() {
        let a = MapAction::PlayerTestMode(PlayerTestMode {
            entering: true,
            skin_url: String::new(),
        });
        assert!(a.is_ephemeral());
        assert_eq!(a.min_proto_version(), 6);
        assert_eq!(a.get_type(), "PlayerTestMode");
        assert_eq!(MAPPING_MSG_PLAYER_TEST_MODE, 23);
        // pre-v6 message types stay ungated
        assert_eq!(MapAction::Ping().min_proto_version(), 0);
        assert_eq!(MapAction::ServerStats(3).min_proto_version(), 0);
    }

    #[test]
    fn proto_version_bytes_mapping() {
        assert_eq!(client_proto_version(&VERSION_BYTES_V5), Some(5));
        assert_eq!(client_proto_version(&VERSION_BYTES_V6), Some(6));
        assert_eq!(client_proto_version(&CURR_VERSION_BYTES), Some(6));
        assert_eq!(client_proto_version(&[0xFF, 0x07, 0x80]), None);
        assert_eq!(client_proto_version(&[0xFF, 0x04, 0x80]), None);
        assert_eq!(client_proto_version(&[0x00, 0x07, 0x80]), None);
    }

    #[test]
    fn wrong_skins_magic_after_blocks_is_invalid() {
        let mut input = Vec::new();
        input.extend_from_slice(b"BLKs");
        input.extend_from_slice(&0u16.to_le_bytes());
        input.extend_from_slice(b"XXXX");
        input.extend_from_slice(&0u16.to_le_bytes());
        input.extend_from_slice(b"ITMs");
        input.extend_from_slice(&0u16.to_le_bytes());
        assert_invalid(&input, "wrong SKNs magic");
    }
}
