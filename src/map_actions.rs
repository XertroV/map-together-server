use crate::{mt_codec::MTDecode, read_lp_string, slice_to_lp_string, StreamErr};


pub const MAPPING_MSG_PLACE: u8 = 1;
pub const MAPPING_MSG_DELETE: u8 = 2;
pub const MAPPING_MSG_RESYNC: u8 = 3;
pub const MAPPING_MSG_SET_SKIN: u8 = 4;
pub const MAPPING_MSG_SET_WAYPOINT: u8 = 5;
pub const MAPPING_MSG_SET_MAPNAME: u8 = 6;
pub const MAPPING_MSG_PLAYER_JOIN: u8 = 7;
pub const MAPPING_MSG_PLAYER_LEAVE: u8 = 8;


#[derive(Debug)]
pub enum MapAction {
    Place(MacroblockSpec),
    Delete(MacroblockSpec),
    Resync(),
    SetSkin(SkinSpec),
    SetWaypoint(WaypointSpec),
    SetMapName(String),
    PlayerJoin {name: String, login: String},
    PlayerLeave {name: String, login: String},
}

#[derive(Debug)]
pub struct MacroblockSpec {
    blocks: Vec<BlockSpec>,
    items: Vec<ItemSpec>,
}

const MAGIC_BLOCKS: &[u8; 4] = b"BLKs";
const MAGIC_ITEMS: &[u8; 4] = b"ITMs";
const MAGIC_SKINS: &[u8; 4] = b"SKNs";

impl MTDecode for MacroblockSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        // expect payload to have blocks, skins, and items prefixed with magic bytes in that order
        let mut cur = 0;
        let mut blocks = Vec::new();
        let mut items = Vec::new();

        // blocks
        if &buf[cur..(cur + 4)] != MAGIC_BLOCKS {
            return Err(StreamErr::InvalidData("expected magic bytes for blocks".to_string()));
        }
        cur += 4;

        let block_count = u16::from_le_bytes([buf[cur], buf[cur + 1]]);
        cur += 2;
        for _ in 0..block_count {
            let block = BlockSpec::decode(&buf[cur..])?;
            cur += 2 + block.name.len() + 4 + 2 + block.author.len() + 12 + 2 + 12 + 1 + 1 + 4 + 4 + 4 + 1;
            if let Some(waypoint) = &block.waypoint {
                cur += 2 + waypoint.tag.len() + 4;
            }
            blocks.push(block);
        }

        // skins (expect none)
        if &buf[cur..(cur + 4)] != MAGIC_SKINS {
            return Err(StreamErr::InvalidData("expected magic bytes for items".to_string()));
        }
        cur += 4;

        let skin_count = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        if skin_count > 0 {
            return Err(StreamErr::InvalidData("skins not implemented".to_string()));
        }

        // items
        if &buf[cur..(cur + 4)] != MAGIC_ITEMS {
            return Err(StreamErr::InvalidData("expected magic bytes for items".to_string()));
        }
        cur += 4;

        let item_count = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        for _ in 0..item_count {
            let item = ItemSpec::decode(&buf[cur..])?;
            cur += 2 + item.name.len() + 4 + 2 + item.author.len() + 12 + 1 + 12 + 4 + 1 + 1 + 1 + 36 + 12 + 1 + 2 + 4 + 4;
            if let Some(waypoint) = &item.waypoint {
                cur += 2 + waypoint.tag.len() + 4;
            }
            items.push(item);
        }
        Ok(MacroblockSpec { blocks, items })
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

#[derive(Debug)]
pub enum SkinApplyType {
    NormBlock { coord: [u32; 3], dir: u8, ident: String },
    GhostBlock { coord: [u32; 3], dir: u8, ident: String },
    FreeBlock { pos: [f32; 3], pyr: [f32; 3], ident: String },
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
            _ => return Err(StreamErr::InvalidData("invalid skin apply type".to_string())),
        };
        Ok(apply_type)
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

#[derive(Debug)]
pub struct BlockSpec {
    decoded_len: usize,
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
        let dir = buf[cur];
        cur += 1;
        let dir2 = buf[cur];
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
        let color = buf[cur];
        cur += 1;
        let lm_qual = buf[cur];
        cur += 1;
        let mobil_ix = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let mobil_variant = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let variant = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let flags = buf[cur];
        cur += 1;
        let waypoint = if buf[cur] == 0 {
            None
        } else {
            Some(WaypointSpec::decode(&buf[cur..])?)
        };
        cur += 1 + waypoint.as_ref().map(|w| w.decoded_len).unwrap_or(0);
        let decoded_len = cur;
        Ok(BlockSpec {
            decoded_len,
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

#[derive(Debug)]
pub struct WaypointSpec {
    decoded_len: usize,
    tag: String,
    order: u32,
}

#[derive(Debug)]
pub struct ItemSpec {
    decoded_len: usize,
    name: String,
    collection: u32,
    author: String,
    coord: [u32; 3],
    dir: u8,
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
        let dir = buf[cur];
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
        let associated_block_ix = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let item_group_on_block = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        let waypoint = if buf[cur] == 0 {
            None
        } else {
            Some(WaypointSpec::decode(&buf[cur..])?)
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

impl MTDecode for WaypointSpec {
    fn decode(buf: &[u8]) -> Result<Self, StreamErr> {
        let mut cur = 0;
        let tag = slice_to_lp_string(&buf[cur..])?;
        cur += 2 + tag.len();
        let order = u32::from_le_bytes([buf[cur], buf[cur + 1], buf[cur + 2], buf[cur + 3]]);
        cur += 4;
        Ok(WaypointSpec { decoded_len: cur, tag, order })
    }
}
