//! Semantic analysis for Wid: name resolution, type checking and lowering to
//! a typed, statement-oriented IR.

mod check;
pub mod index;
pub mod input;
mod interp;
pub mod ir;
pub mod type_info;
pub mod types;
pub mod uses;

pub use check::{check_program, check_program_indexed, is_reserved_type_name};
pub use input::{
    CBinding, CFunction, CRecord, CSkipped, CSlot, CheckOptions, FileInput, PackageId, PackageInput, ProgramInput,
    TARGET_ARCHES, TARGET_OSES, host_arch, host_os, parse_target,
};
