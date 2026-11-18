pub use env_var::{EnvVar, bool_env_var, env_var};
use std::sync::LazyLock;

/// Whether Zed is running in stateless mode.
/// When true, Zed will use in-memory databases instead of persistent storage.
///
/// SOLO: also read under its earlier name, `ZED_STATELESS`, which Solo's
/// docs and scripts used.
pub static SOLO_STATELESS: LazyLock<bool> = LazyLock::new(|| {
    static NEW: LazyLock<bool> = bool_env_var!("SOLO_STATELESS");
    static OLD: LazyLock<bool> = bool_env_var!("ZED_STATELESS");
    *NEW || *OLD
});
