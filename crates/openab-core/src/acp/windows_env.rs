//! Shared Windows process environment allow-list for ACP spawn and the shell tool.

/// Shared Windows runtime environment keys copied into child processes.
///
/// ACP agent spawn (`openab-core`) and `openab-agent` shell execution must use
/// this same allow-list so a future addition cannot silently diverge between
/// the two layers. APP/ LOCALAPPData are intentionally excluded: per-user
/// application-data paths may contain credentials and must not become an
/// implicit agent read surface after prompt injection.
pub const WINDOWS_RUNTIME_ENV_KEYS: &[&str] = &[
    "SystemDrive",
    "WINDIR",
    "ComSpec",
    "PATHEXT",
    "TEMP",
    "TMP",
    "HOMEDRIVE",
    "HOMEPATH",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "PSModulePath",
];
