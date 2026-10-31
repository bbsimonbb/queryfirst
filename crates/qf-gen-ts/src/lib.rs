//! qf-gen-ts: provider-neutral TypeScript type rendering, byte-equivalent to pgtyped's.

mod collate;
mod mapping;
mod types;

pub use collate::locale_compare;
pub use mapping::TsType;
pub use types::{
    GenDiagnostic, RenderedTypes, TsOptions, TypeAllocator, TypeOverrideOpt, render_types,
};
