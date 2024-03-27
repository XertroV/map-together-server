use std::sync::OnceLock;

// pub DUMP_MBS: OnceCell<bool> = OnceCell::new();
pub static DUMP_MBS: OnceLock<bool> = OnceLock::new();
pub const META_BYTES: usize = 46;
