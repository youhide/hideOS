//! Recipes for hideforge: what a recipe file says, whether it says it
//! correctly, what it depends on, and the input hash that names its output.
//!
//! The format is specified in `docs/RECIPE_FORMAT.md` and the model in
//! `docs/HIDEFORGE.md`. Nothing here builds anything or touches a namespace;
//! that is the `hideforge` binary's job, and the split is what lets all of the
//! logic below be tested on any host.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod error;
mod hash;
mod recipe;
mod set;

pub use error::Error;
pub use hash::{Arch, HashContext, InputHash};
pub use recipe::{Build, Depends, Environment, Package, Recipe, Source, Stage, Vendor};
pub use set::{Entry, RecipeSet};
