//! The platform web view behind a browser tab. WebView2 on Windows; elsewhere
//! there is no embedded browser and creation fails.

use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use futures::channel::mpsc::UnboundedSender;
use gpui::{Bounds, Pixels, Window};

#[cfg(target_os = "windows")]
pub use crate::webview2::{EnvironmentConfig, ForwardedKeys, HostEvent, NewWindowRequest};

#[cfg(not(target_os = "windows"))]
mod stub {
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[derive(Debug)]
    pub enum HostEvent {
        Ready,
        Failed(String),
        SourceChanged(String),
        TitleChanged(String),
        HistoryChanged {
            can_go_back: bool,
            can_go_forward: bool,
        },
        Loading(bool),
        NewWindow(NewWindowRequest),
        GotFocus,
        WebMessage(String),
    }

    #[derive(Debug)]
    pub struct NewWindowRequest {
        pub url: String,
    }

    #[derive(Default)]
    pub struct ForwardedKeys {
        pub keys: HashSet<(gpui::Modifiers, String)>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct EnvironmentConfig {
        pub user_data_dir: PathBuf,
        pub remote_debugging_port: Option<u16>,
    }
}
#[cfg(not(target_os = "windows"))]
pub use stub::{EnvironmentConfig, ForwardedKeys, HostEvent, NewWindowRequest};

pub type Callback = Box<dyn FnOnce(Result<String>)>;

/// Whether this computer can show web pages in Solo: `Err` explains why not.
pub fn runtime_available() -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        crate::webview2::runtime_version().map(|_| ())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(anyhow::anyhow!(
            "Solo's browser is only available on Windows"
        ))
    }
}

pub struct Host {
    #[cfg(target_os = "windows")]
    inner: crate::webview2::WebViewHost,
}

impl Host {
    pub fn new(
        window: &Window,
        config: &EnvironmentConfig,
        url: Option<String>,
        adopt: Option<NewWindowRequest>,
        events: UnboundedSender<HostEvent>,
        forwarded: Rc<RefCell<ForwardedKeys>>,
    ) -> Result<Self> {
        #[cfg(target_os = "windows")]
        {
            use anyhow::Context as _;
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let handle = HasWindowHandle::window_handle(window)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                anyhow::bail!("not a Win32 window");
            };
            let parent = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);
            let inner =
                crate::webview2::WebViewHost::new(parent, config, url, adopt, events, forwarded)
                    .context("creating the web view")?;
            Ok(Self { inner })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (window, config, url, adopt, events, forwarded);
            Err(anyhow::anyhow!(
                "Solo's browser is only available on Windows"
            ))
        }
    }

    /// Moves the page over `bounds` (window coordinates), leaving out `holes`,
    /// or hides it.
    pub fn place(&self, placement: Option<(Bounds<Pixels>, Vec<Bounds<Pixels>>)>, scale: f32) {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::RECT;
            let to_rect = |bounds: &Bounds<Pixels>| RECT {
                left: (f32::from(bounds.origin.x) * scale).round() as i32,
                top: (f32::from(bounds.origin.y) * scale).round() as i32,
                right: (f32::from(bounds.origin.x + bounds.size.width) * scale).round() as i32,
                bottom: (f32::from(bounds.origin.y + bounds.size.height) * scale).round() as i32,
            };
            let placement = placement.and_then(|(bounds, holes)| {
                let rect = to_rect(&bounds);
                (rect.right > rect.left && rect.bottom > rect.top).then(|| {
                    crate::webview2::Placement {
                        rect,
                        holes: holes.iter().map(to_rect).collect(),
                    }
                })
            });
            self.inner.place(placement);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = (placement, scale);
    }

    pub fn navigate(&self, url: &str) {
        #[cfg(target_os = "windows")]
        self.inner.navigate(url);
        #[cfg(not(target_os = "windows"))]
        let _ = url;
    }

    pub fn go_back(&self) {
        #[cfg(target_os = "windows")]
        self.inner.go_back();
    }

    pub fn go_forward(&self) {
        #[cfg(target_os = "windows")]
        self.inner.go_forward();
    }

    pub fn reload(&self, bypass_cache: bool) {
        #[cfg(target_os = "windows")]
        self.inner.reload(bypass_cache);
        #[cfg(not(target_os = "windows"))]
        let _ = bypass_cache;
    }

    pub fn open_devtools(&self) {
        #[cfg(target_os = "windows")]
        self.inner.open_devtools();
    }

    pub fn find(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            self.inner.find()
        }
        #[cfg(not(target_os = "windows"))]
        false
    }

    pub fn call_devtools(&self, method: &str, params_json: &str, callback: Callback) {
        #[cfg(target_os = "windows")]
        self.inner.call_devtools(method, params_json, callback);
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (method, params_json);
            callback(Err(anyhow::anyhow!("unsupported")));
        }
    }

    pub fn execute_script(&self, script: &str, callback: Callback) {
        #[cfg(target_os = "windows")]
        self.inner.execute_script(script, callback);
        #[cfg(not(target_os = "windows"))]
        {
            let _ = script;
            callback(Err(anyhow::anyhow!("unsupported")));
        }
    }

    pub fn parent_moved(&self) {
        #[cfg(target_os = "windows")]
        self.inner.parent_moved();
    }

    pub fn focus(&self) {
        #[cfg(target_os = "windows")]
        self.inner.focus();
    }

    pub fn release_focus(&self) {
        #[cfg(target_os = "windows")]
        self.inner.release_focus();
    }
}
