//! Architecture vocabulary shared by the inspector and the runner selector.

use serde::Serialize;
use std::fmt;

/// CPU architecture of the Linux host we are running on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostArch {
    X86_64,
    Aarch64,
    /// Anything else (riscv64, ppc64le, ...). Not supported by this prototype.
    Other,
}

impl HostArch {
    pub const ALL: [HostArch; 3] = [HostArch::X86_64, HostArch::Aarch64, HostArch::Other];

    /// The architecture this binary was compiled for.
    pub fn current() -> HostArch {
        Self::from_name(std::env::consts::ARCH)
    }

    pub fn from_name(name: &str) -> HostArch {
        match name {
            "x86_64" | "amd64" | "x64" => HostArch::X86_64,
            "aarch64" | "arm64" => HostArch::Aarch64,
            _ => HostArch::Other,
        }
    }
}

impl fmt::Display for HostArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HostArch::X86_64 => "x86_64",
            HostArch::Aarch64 => "aarch64",
            HostArch::Other => "unsupported architecture",
        })
    }
}

/// What kind of Windows code a file contains, as far as running it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExeArch {
    /// 32-bit x86 (IMAGE_FILE_MACHINE_I386).
    I386,
    /// 64-bit x86 (IMAGE_FILE_MACHINE_AMD64).
    X86_64,
    /// Pure ARM64 (IMAGE_FILE_MACHINE_ARM64).
    Arm64,
    /// ARM64EC: ARM64 code using the x64 ABI, may contain x64 code
    /// (AMD64 machine field + CHPE metadata, or IMAGE_FILE_MACHINE_ARM64EC).
    Arm64ec,
    /// ARM64X hybrid image (ARM64 machine field + CHPE metadata).
    Arm64x,
    /// A Windows Installer package (.msi). Run through `msiexec`; may contain
    /// x86 custom-action DLLs.
    MsiPackage,
    /// Some other machine type (Itanium, MIPS, ...).
    Unknown(u16),
}

impl ExeArch {
    /// Every variant, for exhaustive tests.
    pub const ALL: [ExeArch; 7] = [
        ExeArch::I386,
        ExeArch::X86_64,
        ExeArch::Arm64,
        ExeArch::Arm64ec,
        ExeArch::Arm64x,
        ExeArch::MsiPackage,
        ExeArch::Unknown(0x0200),
    ];

    pub fn is_x86_family(self) -> bool {
        matches!(self, ExeArch::I386 | ExeArch::X86_64)
    }
}

impl fmt::Display for ExeArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExeArch::I386 => f.write_str("x86 (32-bit)"),
            ExeArch::X86_64 => f.write_str("x86-64"),
            ExeArch::Arm64 => f.write_str("ARM64"),
            ExeArch::Arm64ec => f.write_str("ARM64EC"),
            ExeArch::Arm64x => f.write_str("ARM64X"),
            ExeArch::MsiPackage => f.write_str("Windows Installer package"),
            ExeArch::Unknown(m) => write!(f, "unknown machine 0x{m:04x}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names() {
        assert_eq!(HostArch::from_name("x86_64"), HostArch::X86_64);
        assert_eq!(HostArch::from_name("aarch64"), HostArch::Aarch64);
        assert_eq!(HostArch::from_name("arm64"), HostArch::Aarch64);
        assert_eq!(HostArch::from_name("riscv64"), HostArch::Other);
    }

    #[test]
    fn x86_family() {
        assert!(ExeArch::I386.is_x86_family());
        assert!(ExeArch::X86_64.is_x86_family());
        assert!(!ExeArch::Arm64ec.is_x86_family());
        assert!(!ExeArch::MsiPackage.is_x86_family());
    }
}
