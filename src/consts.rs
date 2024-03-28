use std::sync::OnceLock;

// pub DUMP_MBS: OnceCell<bool> = OnceCell::new();
pub static DUMP_MBS: OnceLock<bool> = OnceLock::new();
pub const META_BYTES: usize = 46;

pub const XERTROV_WSID: &str = "0a2d1bc0-4aaa-4374-b2db-3d561bdab1c9";
