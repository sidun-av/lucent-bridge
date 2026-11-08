//! Lucent Bridge: run Windows programs on Lucent OS without a virtual machine.
//!
//! x86/x86-64 programs run directly on x86-64 hosts through Wine; on AArch64
//! hosts their instructions are translated to ARM by FEX-Emu inside Wine's
//! ARM64EC/WoW64 layer. See README.md for the design.

pub mod appid;
pub mod arch;
pub mod compat;
pub mod desktop;
pub mod inspect;
pub mod installer;
pub mod integrate;
pub mod lnk;
pub mod pe;
pub mod prefix;
pub mod runner;
#[doc(hidden)]
pub mod testpe;
