//! Runtime for Rust components speaking WNA. Generated bindings
//! (`wna::generate!`) build on this crate; application code rarely touches it
//! directly beyond [`Trap`] and [`catch`].

pub mod abi;
pub mod component;
pub mod compose;
pub mod handle;
pub mod lift;
pub mod trap;

pub use trap::{Trap, catch};
pub use wna_macro::generate;

#[doc(hidden)]
pub mod __private {
    pub use bitflags;
    #[cfg(target_family = "wasm")]
    pub use wit_bindgen;
}
