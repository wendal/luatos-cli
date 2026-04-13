// LuatOS SOC file parser — unpack/pack/info.json handling.
//
// SOC files are archives (ZIP for Air8101, 7z for others) containing:
//   - info.json: metadata (chip type, ROM path, addresses, baud rates)
//   - ROM binary (e.g. luatos.bin)
//   - Optional: air602_flash.exe, luac, etc.

pub mod combine;
mod info;
pub mod pack;
mod unpack;

pub use combine::combine_ec7xx_soc;
pub use info::*;
pub use pack::*;
pub use unpack::*;
