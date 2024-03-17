

pub const MAPPING_MSG_PLACE: u8 = 1;
pub const MAPPING_MSG_DELETE: u8 = 2;
pub const MAPPING_MSG_RESYNC: u8 = 3;
pub const MAPPING_MSG_SET_SKIN: u8 = 4;
pub const MAPPING_MSG_SET_WAYPOINT: u8 = 5;
pub const MAPPING_MSG_SET_MAPNAME: u8 = 6;



enum MapAction {
    Place(MacroblockSpec),
    Delete(MacroblockSpec),
    Resync(),
    SetSkin(SkinSpec),
    SetWaypoint(WaypointSpec),
    SetMapName(String),
}

struct MacroblockSpec {
    blocks: Vec<BlockSpec>,
    items: Vec<ItemSpec>,
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

pub struct SkinSpec {
    name: String,
    apply_type: SkinApplyType,

}

enum SkinApplyType {

}

pub struct BlockSpec {
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

pub struct WaypointSpec {
    tag: String,
    order: u32,
}

pub struct ItemSpec {
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
