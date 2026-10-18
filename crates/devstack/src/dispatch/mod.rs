//! What each command does, by area: `main` hands a parsed command to the
//! area it belongs to. The command line itself is defined in `main.rs`.

pub(crate) mod hub;
pub(crate) mod internal;
pub(crate) mod lane;
pub(crate) mod myself;
pub(crate) mod pool;
pub(crate) mod services;
pub(crate) mod skills;
pub(crate) mod stack;

/// An area was handed a command of another one: `main` routes every
/// command to exactly one area.
fn misrouted() -> ! {
    unreachable!("main routes every command to the area that handles it")
}
