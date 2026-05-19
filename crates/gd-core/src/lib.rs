#![warn(clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::cast_precision_loss
)]

pub mod db;
pub mod error;
pub mod frecency;
pub mod index;
pub mod path;
