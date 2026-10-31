//! qf-gen-ts: provider-neutral TypeScript type rendering, byte-equivalent to pgtyped's.

mod binding;
mod collate;
mod file;
mod mapping;
mod pgtyped_binding;
mod types;

pub use binding::RuntimeBinding;
pub use collate::locale_compare;
pub use file::{OutputFile, generate, output_path};
pub use mapping::TsType;
pub use pgtyped_binding::{PgtypedBinding, meta_ir};
pub use types::{
    GenDiagnostic, RenderedTypes, TsOptions, TypeAllocator, TypeOverrideOpt, render_types,
};
