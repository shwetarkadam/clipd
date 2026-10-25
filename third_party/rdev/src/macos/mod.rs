mod common;
mod display;
#[cfg(feature = "unstable_grab")]
mod grab;
mod keyboard;
mod keycodes;
mod listen;
mod simulate;

pub use crate::macos::display::display_size;
#[cfg(feature = "unstable_grab")]
pub use crate::macos::grab::grab;
pub use crate::macos::keyboard::Keyboard;
// clipd patch: event.name is None on macOS (see keyboard.rs), so clipd maps
// key codes to characters itself and needs the code behind a Key.
pub use crate::macos::keycodes::code_from_key;
pub use crate::macos::listen::listen;
pub use crate::macos::simulate::simulate;
