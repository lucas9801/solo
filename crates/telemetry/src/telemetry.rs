//! SOLO: Zed's telemetry events, as a no-op. Zed sent these to zed.dev; Solo
//! sends no telemetry. The macro stays so that the many places in Zed's code
//! that record an event don't change.

/// Records nothing. Zed queued a telemetry event here and sent it to zed.dev.
///
/// The name and properties are still evaluated (by reference, as Zed did), so
/// the calls compile and behave the same apart from sending nothing.
///
/// ```
/// # let url = "https://example.com";
/// telemetry::event!("Keymap Changed", version = "1.0.0");
/// telemetry::event!("Documentation Viewed", url, source = "Extension Upsell");
/// ```
#[macro_export]
macro_rules! event {
    ($name:expr) => {{
        let _ = &$name;
    }};
    ($name:expr, $($key:ident $(= $value:expr)?),+ $(,)?) => {{
        let _ = &$name;
        $(
            let _ = &$crate::serialize_property!($key $(= $value)?);
        )+
    }};
}

#[macro_export]
macro_rules! serialize_property {
    ($key:ident) => {
        $key
    };
    ($key:ident = $value:expr) => {
        $value
    };
}
