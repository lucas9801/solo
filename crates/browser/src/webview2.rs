//! A WebView2 control hosted in a native child window of a GPUI window.
//!
//! GPUI draws through DirectComposition into the top-level window, and the
//! web page lives in a child window placed over the element that shows it.
//! The owner keeps that child window in step with the GPUI layout: it moves
//! and resizes it after each frame, hides it when the element was not drawn,
//! and clips out whatever GPUI drew on top (menus, popovers, modals, toasts)
//! with a window region, so those stay visible and clickable.
//!
//! The environment is created without `WebView2Loader.dll`: the runtime's own
//! `EmbeddedBrowserWebView.dll` is located through the EdgeUpdate registry key
//! (the same thing the loader does), so a missing runtime is an ordinary error
//! instead of a missing import at process start.

use std::{
    cell::RefCell,
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::OnceLock,
};

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc::UnboundedSender;
use gpui::Modifiers;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CallDevToolsProtocolMethodCompletedHandler,
    CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, DocumentTitleChangedEventHandler,
    ExecuteScriptCompletedHandler, FocusChangedEventHandler, HistoryChangedEventHandler,
    Microsoft::Web::WebView2::Win32::*, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler, SourceChangedEventHandler,
    WebMessageReceivedEventHandler, take_pwstr,
};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, LPARAM, RECT, WPARAM},
        Graphics::Gdi::{CombineRgn, CreateRectRgn, DeleteObject, HRGN, RGN_DIFF, SetWindowRgn},
        System::LibraryLoader::{GetProcAddress, LoadLibraryW},
        UI::{
            Input::KeyboardAndMouse::{
                GetFocus, GetKeyState, MAPVK_VK_TO_CHAR, MapVirtualKeyW, SetFocus, VK_CONTROL,
                VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
            },
            WindowsAndMessaging::{
                CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, IsChild,
                PostMessageW, RegisterClassW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER,
                SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WM_KEYDOWN, WM_SYSKEYDOWN, WNDCLASSW,
                WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
            },
        },
    },
    core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR, w},
};

/// Something the web view reports to its owner. Delivered through a channel,
/// so the owner handles it outside of WebView2's callback.
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
    /// The page opened a window (a `target=_blank` link, `window.open`, or a
    /// DevTools client's new page): show `request` in a new tab.
    NewWindow(NewWindowRequest),
    GotFocus,
    WebMessage(String),
}

/// A window the page asked for, held open until a new tab's web view takes
/// it over (so the opener keeps its handle, and DevTools clients see the new
/// page as theirs). Dropping it unused cancels the window.
pub struct NewWindowRequest {
    pub url: String,
    args: ICoreWebView2NewWindowRequestedEventArgs,
    deferral: Option<ICoreWebView2Deferral>,
}

impl std::fmt::Debug for NewWindowRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewWindowRequest")
            .field("url", &self.url)
            .finish()
    }
}

impl NewWindowRequest {
    fn complete(&mut self, webview: Option<&ICoreWebView2>) {
        let Some(deferral) = self.deferral.take() else {
            return;
        };
        unsafe {
            match webview {
                Some(webview) => self.args.SetNewWindow(webview).log_failure("SetNewWindow"),
                None => self.args.SetHandled(true).log_failure("SetHandled"),
            }
            deferral.Complete().log_failure("Complete");
        }
    }
}

impl Drop for NewWindowRequest {
    fn drop(&mut self) {
        self.complete(None);
    }
}

/// The keystrokes Solo takes over while the page has keyboard focus.
/// Matched by modifiers and the unshifted key name, as GPUI names keys.
#[derive(Default)]
pub struct ForwardedKeys {
    pub keys: HashSet<(Modifiers, String)>,
}

/// Settings fixed for the life of the shared browser process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentConfig {
    pub user_data_dir: PathBuf,
    pub remote_debugging_port: Option<u16>,
}

type EnvWaiter = Box<dyn FnOnce(Result<ICoreWebView2Environment, String>)>;

enum EnvState {
    Idle,
    Pending(Vec<EnvWaiter>),
    Ready(ICoreWebView2Environment),
    Failed(String),
}

thread_local! {
    static ENVIRONMENT: RefCell<EnvState> = const { RefCell::new(EnvState::Idle) };
}

/// Returns the installed runtime version, or an error when the WebView2 runtime
/// is not installed.
pub fn runtime_version() -> Result<String> {
    find_runtime().map(|runtime| runtime.version)
}

struct Runtime {
    version: String,
    client_dll: PathBuf,
}

const WEBVIEW2_CLIENT_ID: &str =
    r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

fn find_runtime() -> Result<Runtime> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "x64"
    };
    let candidates = [
        (
            windows_registry::LOCAL_MACHINE,
            format!(r"SOFTWARE\WOW6432Node\{WEBVIEW2_CLIENT_ID}"),
        ),
        (
            windows_registry::LOCAL_MACHINE,
            format!(r"SOFTWARE\{WEBVIEW2_CLIENT_ID}"),
        ),
        (
            windows_registry::CURRENT_USER,
            format!(r"SOFTWARE\{WEBVIEW2_CLIENT_ID}"),
        ),
    ];
    for (root, path) in candidates {
        let Ok(key) = root.open(&path) else {
            continue;
        };
        let (Ok(version), Ok(location)) = (key.get_string("pv"), key.get_string("location")) else {
            continue;
        };
        if version.is_empty() || version == "0.0.0.0" {
            continue;
        }
        let client_dll = Path::new(&location)
            .join(&version)
            .join("EBWebView")
            .join(arch)
            .join("EmbeddedBrowserWebView.dll");
        if client_dll.is_file() {
            return Ok(Runtime {
                version,
                client_dll,
            });
        }
    }
    Err(anyhow!(
        "the Microsoft Edge WebView2 runtime is not installed"
    ))
}

type CreateInternal = unsafe extern "system" fn(
    check_running_instance: bool,
    runtime_type: i32,
    user_data_dir: PCWSTR,
    options: *mut core::ffi::c_void,
    handler: *mut core::ffi::c_void,
) -> windows_core::HRESULT;

type CreateWithOptions = unsafe extern "system" fn(
    browser_folder: PCWSTR,
    user_data_dir: PCWSTR,
    options: *mut core::ffi::c_void,
    handler: *mut core::ffi::c_void,
) -> windows_core::HRESULT;

fn start_environment(config: &EnvironmentConfig) -> Result<()> {
    let runtime = find_runtime()?;
    std::fs::create_dir_all(&config.user_data_dir)
        .with_context(|| format!("creating {}", config.user_data_dir.display()))?;

    let options = CoreWebView2EnvironmentOptions::default();
    let mut arguments =
        String::from("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection");
    if let Some(port) = config.remote_debugging_port {
        arguments.push_str(&format!(
            " --remote-debugging-port={port} --remote-debugging-address=127.0.0.1"
        ));
    }
    unsafe {
        options.set_additional_browser_arguments(arguments);
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let handler =
        CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|result, environment| {
            let outcome = match (result, environment) {
                (Ok(()), Some(environment)) => Ok(environment),
                (Err(error), _) => Err(format!("{error}")),
                (Ok(()), None) => Err("no environment".to_string()),
            };
            finish_environment(outcome);
            Ok(())
        }));
    let user_data_dir = HSTRING::from(config.user_data_dir.as_os_str());

    unsafe {
        // The official loader, if someone put it next to Solo, then the runtime directly.
        if let Ok(loader) = LoadLibraryW(w!("WebView2Loader.dll"))
            && let Some(create) = GetProcAddress(
                loader,
                windows::core::s!("CreateCoreWebView2EnvironmentWithOptions"),
            )
        {
            let create: CreateWithOptions = std::mem::transmute(create);
            return create(
                PCWSTR::null(),
                PCWSTR(user_data_dir.as_ptr()),
                options.as_raw(),
                handler.as_raw(),
            )
            .ok()
            .context("CreateCoreWebView2EnvironmentWithOptions");
        }

        let module: HMODULE = LoadLibraryW(&HSTRING::from(runtime.client_dll.as_os_str()))
            .with_context(|| format!("loading {}", runtime.client_dll.display()))?;
        let create = GetProcAddress(
            module,
            windows::core::s!("CreateWebViewEnvironmentWithOptionsInternal"),
        )
        .context("the WebView2 runtime has no environment entry point")?;
        let create: CreateInternal = std::mem::transmute(create);
        create(
            true,
            0,
            PCWSTR(user_data_dir.as_ptr()),
            options.as_raw(),
            handler.as_raw(),
        )
        .ok()
        .context("CreateWebViewEnvironmentWithOptionsInternal")
    }
}

fn finish_environment(outcome: Result<ICoreWebView2Environment, String>) {
    let waiters = ENVIRONMENT.with(|state| {
        let mut state = state.borrow_mut();
        let previous = std::mem::replace(
            &mut *state,
            match &outcome {
                Ok(environment) => EnvState::Ready(environment.clone()),
                Err(error) => EnvState::Failed(error.clone()),
            },
        );
        match previous {
            EnvState::Pending(waiters) => waiters,
            _ => Vec::new(),
        }
    });
    if let Err(error) = &outcome {
        log::error!("WebView2 environment: {error}");
    }
    for waiter in waiters {
        waiter(outcome.clone());
    }
}

/// Calls `callback` with the shared environment, creating it on first use with
/// `config`. Later configs are ignored until Solo restarts.
fn environment_or_wait(config: &EnvironmentConfig, callback: EnvWaiter) {
    let settled = ENVIRONMENT.with(|state| match &*state.borrow() {
        EnvState::Ready(environment) => Some(Ok(environment.clone())),
        EnvState::Failed(error) => Some(Err(error.clone())),
        EnvState::Idle | EnvState::Pending(_) => None,
    });
    if let Some(result) = settled {
        callback(result);
        return;
    }
    let start = ENVIRONMENT.with(|state| {
        let mut state = state.borrow_mut();
        match &mut *state {
            EnvState::Pending(waiters) => {
                waiters.push(callback);
                false
            }
            _ => {
                *state = EnvState::Pending(vec![callback]);
                true
            }
        }
    });
    if start {
        if let Err(error) = start_environment(config) {
            finish_environment(Err(format!("{error:#}")));
        }
    }
}

fn host_class() -> PCWSTR {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    let class = w!("SoloWebViewHost");
    REGISTERED.get_or_init(|| unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(host_wnd_proc),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
    });
    class
}

unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Where the web view goes, in physical pixels of the parent's client area.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub rect: RECT,
    /// Parts of `rect` that GPUI drew something on top of.
    pub holes: Vec<RECT>,
}

#[derive(Default)]
struct Inner {
    controller: Option<ICoreWebView2Controller>,
    webview: Option<ICoreWebView2>,
    pending_url: Option<String>,
    /// A window the opener asked for, to become this web view.
    adopt: Option<NewWindowRequest>,
    pending_focus: bool,
    placement: Option<Placement>,
    closed: bool,
}

pub struct WebViewHost {
    parent: HWND,
    container: HWND,
    inner: Rc<RefCell<Inner>>,
}

impl WebViewHost {
    pub fn new(
        parent: HWND,
        config: &EnvironmentConfig,
        url: Option<String>,
        adopt: Option<NewWindowRequest>,
        events: UnboundedSender<HostEvent>,
        forwarded: Rc<RefCell<ForwardedKeys>>,
    ) -> Result<Self> {
        let container = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                host_class(),
                w!(""),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                None,
                None,
            )
        }
        .context("creating the web view host window")?;

        let inner = Rc::new(RefCell::new(Inner {
            // An adopted window loads what its opener asked for.
            pending_url: if adopt.is_some() { None } else { url },
            adopt,
            ..Default::default()
        }));

        let weak_inner = Rc::downgrade(&inner);
        environment_or_wait(
            config,
            Box::new(move |environment| {
                let Some(inner) = weak_inner.upgrade() else {
                    return;
                };
                match environment {
                    Ok(environment) => {
                        if let Err(error) = create_controller(
                            &environment,
                            parent,
                            container,
                            inner,
                            events.clone(),
                            forwarded,
                        ) {
                            events
                                .unbounded_send(HostEvent::Failed(format!("{error:#}")))
                                .ok();
                        }
                    }
                    Err(error) => {
                        events.unbounded_send(HostEvent::Failed(error)).ok();
                    }
                }
            }),
        );

        Ok(Self {
            parent,
            container,
            inner,
        })
    }

    fn webview(&self) -> Option<ICoreWebView2> {
        self.inner.borrow().webview.clone()
    }

    pub fn navigate(&self, url: &str) {
        match self.webview() {
            Some(webview) => unsafe {
                webview
                    .Navigate(&HSTRING::from(url))
                    .log_failure("Navigate");
            },
            None => self.inner.borrow_mut().pending_url = Some(url.to_string()),
        }
    }

    pub fn go_back(&self) {
        if let Some(webview) = self.webview() {
            unsafe { webview.GoBack().log_failure("GoBack") };
        }
    }

    pub fn go_forward(&self) {
        if let Some(webview) = self.webview() {
            unsafe { webview.GoForward().log_failure("GoForward") };
        }
    }

    pub fn reload(&self, bypass_cache: bool) {
        if bypass_cache {
            self.call_devtools(
                "Page.reload",
                r#"{"ignoreCache":true}"#,
                Box::new(|result| {
                    if let Err(error) = result {
                        log::warn!("hard reload: {error:#}");
                    }
                }),
            );
        } else if let Some(webview) = self.webview() {
            unsafe { webview.Reload().log_failure("Reload") };
        }
    }

    pub fn open_devtools(&self) {
        if let Some(webview) = self.webview() {
            unsafe {
                webview
                    .OpenDevToolsWindow()
                    .log_failure("OpenDevToolsWindow")
            };
        }
    }

    /// Opens WebView2's own find bar in the page.
    pub fn find(&self) -> bool {
        let Some(webview) = self.webview() else {
            return false;
        };
        let Some(controller) = self.inner.borrow().controller.clone() else {
            return false;
        };
        unsafe {
            let (Ok(webview28), Ok(environment)) = (
                webview.cast::<ICoreWebView2_28>(),
                webview
                    .cast::<ICoreWebView2_2>()
                    .and_then(|webview| webview.Environment()),
            ) else {
                return false;
            };
            let Ok(environment15) = environment.cast::<ICoreWebView2Environment15>() else {
                return false;
            };
            let (Ok(find), Ok(options)) = (webview28.Find(), environment15.CreateFindOptions())
            else {
                return false;
            };
            controller
                .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                .log_failure("MoveFocus");
            options.SetSuppressDefaultFindDialog(false).ok();
            find.Start(
                &options,
                &webview2_com::FindStartCompletedHandler::create(Box::new(|_| Ok(()))),
            )
            .is_ok()
        }
    }

    /// Calls a Chrome DevTools Protocol method on the page. The callback gets
    /// the JSON result.
    pub fn call_devtools(
        &self,
        method: &str,
        params_json: &str,
        callback: Box<dyn FnOnce(Result<String>)>,
    ) {
        let Some(webview) = self.webview() else {
            callback(Err(anyhow!("the page is not ready")));
            return;
        };
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                callback(result.map(|_| json).map_err(|error| anyhow!("{error}")));
                Ok(())
            }));
        unsafe {
            if let Err(error) = webview.CallDevToolsProtocolMethod(
                &HSTRING::from(method),
                &HSTRING::from(params_json),
                &handler,
            ) {
                log::warn!("CallDevToolsProtocolMethod {method}: {error}");
            }
        }
    }

    /// Runs a script in the page; the callback gets its result as JSON.
    pub fn execute_script(&self, script: &str, callback: Box<dyn FnOnce(Result<String>)>) {
        let Some(webview) = self.webview() else {
            callback(Err(anyhow!("the page is not ready")));
            return;
        };
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            callback(result.map(|_| json).map_err(|error| anyhow!("{error}")));
            Ok(())
        }));
        unsafe {
            webview
                .ExecuteScript(&HSTRING::from(script), &handler)
                .log_failure("ExecuteScript");
        }
    }

    /// Moves the web view to `placement`, or hides it when `None`.
    pub fn place(&self, placement: Option<Placement>) {
        let mut inner = self.inner.borrow_mut();
        if inner.placement == placement {
            return;
        }
        inner.placement = placement.clone();
        let controller = inner.controller.clone();
        drop(inner);
        apply_placement(self.container, controller.as_ref(), placement.as_ref());
    }

    /// Tells the page its window moved, so its popups (like a `<select>`'s
    /// list) open in the right place.
    pub fn parent_moved(&self) {
        let controller = self.inner.borrow().controller.clone();
        if let Some(controller) = controller {
            unsafe {
                controller
                    .NotifyParentWindowPositionChanged()
                    .log_failure("NotifyParentWindowPositionChanged");
            }
        }
    }

    /// Gives the page keyboard focus.
    pub fn focus(&self) {
        let controller = self.inner.borrow().controller.clone();
        match controller {
            Some(controller) => unsafe {
                controller
                    .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                    .log_failure("MoveFocus");
            },
            None => self.inner.borrow_mut().pending_focus = true,
        }
    }

    /// Whether the page (or anything else inside the host window) has
    /// Win32 keyboard focus.
    pub fn has_focus(&self) -> bool {
        unsafe {
            let focused = GetFocus();
            !focused.is_invalid()
                && (focused == self.container || IsChild(self.container, focused).as_bool())
        }
    }

    /// Hands Win32 keyboard focus back to the GPUI window.
    pub fn release_focus(&self) {
        if self.has_focus() {
            unsafe {
                SetFocus(Some(self.parent)).ok();
            }
        }
    }
}

impl Drop for WebViewHost {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.closed = true;
        if let Some(controller) = inner.controller.take() {
            unsafe { controller.Close().log_failure("Close") };
        }
        inner.webview = None;
        drop(inner);
        unsafe {
            DestroyWindow(self.container).ok();
        }
    }
}

fn apply_placement(
    container: HWND,
    controller: Option<&ICoreWebView2Controller>,
    placement: Option<&Placement>,
) {
    unsafe {
        let Some(placement) = placement else {
            if let Some(controller) = controller {
                controller.SetIsVisible(false).log_failure("SetIsVisible");
            }
            let _ = ShowWindow(container, SW_HIDE);
            return;
        };
        let rect = placement.rect;
        let width = (rect.right - rect.left).max(0);
        let height = (rect.bottom - rect.top).max(0);
        SetWindowPos(
            container,
            None,
            rect.left,
            rect.top,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
        .ok();

        // The region is in the container's own coordinates.
        let region: Option<HRGN> = if placement.holes.is_empty() {
            None
        } else {
            let region = CreateRectRgn(0, 0, width, height);
            for hole in &placement.holes {
                let hole = CreateRectRgn(
                    hole.left - rect.left,
                    hole.top - rect.top,
                    hole.right - rect.left,
                    hole.bottom - rect.top,
                );
                CombineRgn(Some(region), Some(region), Some(hole), RGN_DIFF);
                let _ = DeleteObject(hole.into());
            }
            Some(region)
        };
        // The system owns the region after this call.
        SetWindowRgn(container, region, true);

        if let Some(controller) = controller {
            controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                })
                .log_failure("SetBounds");
            controller.SetIsVisible(true).log_failure("SetIsVisible");
        }
        let _ = ShowWindow(container, SW_SHOWNA);
    }
}

fn create_controller(
    environment: &ICoreWebView2Environment,
    parent: HWND,
    container: HWND,
    inner: Rc<RefCell<Inner>>,
    events: UnboundedSender<HostEvent>,
    forwarded: Rc<RefCell<ForwardedKeys>>,
) -> Result<()> {
    let weak_inner = Rc::downgrade(&inner);
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            let Some(inner) = weak_inner.upgrade() else {
                if let Some(controller) = controller {
                    unsafe { controller.Close().ok() };
                }
                return Ok(());
            };
            let controller = match (result, controller) {
                (Ok(()), Some(controller)) => controller,
                (Err(error), _) => {
                    events
                        .unbounded_send(HostEvent::Failed(format!("{error}")))
                        .ok();
                    return Ok(());
                }
                (Ok(()), None) => {
                    events
                        .unbounded_send(HostEvent::Failed("no controller".into()))
                        .ok();
                    return Ok(());
                }
            };
            if inner.borrow().closed {
                unsafe { controller.Close().ok() };
                return Ok(());
            }
            if let Err(error) = setup_controller(
                &controller,
                parent,
                container,
                &inner,
                events.clone(),
                forwarded.clone(),
            ) {
                events
                    .unbounded_send(HostEvent::Failed(format!("{error:#}")))
                    .ok();
            }
            Ok(())
        },
    ));
    unsafe {
        environment
            .CreateCoreWebView2Controller(container, &handler)
            .context("CreateCoreWebView2Controller")
    }
}

fn setup_controller(
    controller: &ICoreWebView2Controller,
    parent: HWND,
    container: HWND,
    inner: &Rc<RefCell<Inner>>,
    events: UnboundedSender<HostEvent>,
    forwarded: Rc<RefCell<ForwardedKeys>>,
) -> Result<()> {
    unsafe {
        let webview = controller.CoreWebView2()?;
        let mut token = 0i64;

        if let Ok(settings) = webview.Settings() {
            settings.SetIsStatusBarEnabled(true).ok();
            settings.SetAreDevToolsEnabled(true).ok();
            settings.SetIsZoomControlEnabled(true).ok();
        }

        let tx = events.clone();
        webview.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |webview, _| {
                if let Some(webview) = webview {
                    let mut uri = PWSTR::null();
                    webview.Source(&mut uri)?;
                    tx.unbounded_send(HostEvent::SourceChanged(take_pwstr(uri)))
                        .ok();
                }
                Ok(())
            })),
            &mut token,
        )?;

        let tx = events.clone();
        webview.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(Box::new(move |webview, _| {
                if let Some(webview) = webview {
                    let mut title = PWSTR::null();
                    webview.DocumentTitle(&mut title)?;
                    tx.unbounded_send(HostEvent::TitleChanged(take_pwstr(title)))
                        .ok();
                }
                Ok(())
            })),
            &mut token,
        )?;

        let tx = events.clone();
        webview.add_HistoryChanged(
            &HistoryChangedEventHandler::create(Box::new(move |webview, _| {
                if let Some(webview) = webview {
                    let mut back = BOOL::default();
                    let mut forward = BOOL::default();
                    webview.CanGoBack(&mut back)?;
                    webview.CanGoForward(&mut forward)?;
                    tx.unbounded_send(HostEvent::HistoryChanged {
                        can_go_back: back.as_bool(),
                        can_go_forward: forward.as_bool(),
                    })
                    .ok();
                }
                Ok(())
            })),
            &mut token,
        )?;

        let tx = events.clone();
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, _| {
                tx.unbounded_send(HostEvent::Loading(true)).ok();
                Ok(())
            })),
            &mut token,
        )?;

        let tx = events.clone();
        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_, _| {
                tx.unbounded_send(HostEvent::Loading(false)).ok();
                Ok(())
            })),
            &mut token,
        )?;

        let tx = events.clone();
        webview.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    // The new tab's web view is created after this handler
                    // returns (creating it inside would deadlock).
                    let deferral = args.GetDeferral()?;
                    tx.unbounded_send(HostEvent::NewWindow(NewWindowRequest {
                        url: take_pwstr(uri),
                        args,
                        deferral: Some(deferral),
                    }))
                    .ok();
                }
                Ok(())
            })),
            &mut token,
        )?;

        let tx = events.clone();
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut message = PWSTR::null();
                    if args.TryGetWebMessageAsString(&mut message).is_ok() {
                        tx.unbounded_send(HostEvent::WebMessage(take_pwstr(message)))
                            .ok();
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;

        let tx = events.clone();
        controller.add_GotFocus(
            &FocusChangedEventHandler::create(Box::new(move |_, _| {
                tx.unbounded_send(HostEvent::GotFocus).ok();
                Ok(())
            })),
            &mut token,
        )?;

        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                args.KeyEventKind(&mut kind)?;
                let message = if kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN {
                    WM_KEYDOWN
                } else if kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                    WM_SYSKEYDOWN
                } else {
                    return Ok(());
                };
                let mut vk = 0u32;
                args.VirtualKey(&mut vk)?;
                let Some(key) = forwarded_key_for(vk) else {
                    return Ok(());
                };
                if !forwarded.borrow().keys.contains(&key) {
                    return Ok(());
                }
                let mut lparam = 0i32;
                args.KeyEventLParam(&mut lparam)?;
                args.SetHandled(true)?;
                // GPUI reads the key the usual way, so layouts and repeats work as
                // if the GPUI window had focus.
                PostMessageW(
                    Some(parent),
                    message,
                    WPARAM(vk as usize),
                    LPARAM(lparam as isize),
                )
                .ok();
                Ok(())
            })),
            &mut token,
        )?;

        let (url, focus, adopt) = {
            let mut inner = inner.borrow_mut();
            inner.controller = Some(controller.clone());
            inner.webview = Some(webview.clone());
            (
                inner.pending_url.take(),
                std::mem::take(&mut inner.pending_focus),
                inner.adopt.take(),
            )
        };
        if let Some(mut adopt) = adopt {
            adopt.complete(Some(&webview));
        }
        let placement = inner.borrow().placement.clone();
        if placement.is_none() {
            // A web view with no size loads nothing, and a tab opened in the
            // background (by an agent, say) may not be shown for a while.
            controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: 1280,
                    bottom: 800,
                })
                .log_failure("SetBounds");
        }
        apply_placement(container, Some(controller), placement.as_ref());
        if let Some(url) = url {
            webview
                .Navigate(&HSTRING::from(url))
                .log_failure("Navigate");
        }
        if focus {
            controller
                .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                .log_failure("MoveFocus");
        }
        events.unbounded_send(HostEvent::Ready).ok();
    }
    Ok(())
}

/// The modifiers held now and the GPUI name of `vk` without shift applied, or
/// `None` for keys Solo never takes from the page.
fn forwarded_key_for(vk: u32) -> Option<(Modifiers, String)> {
    let down = |key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe {
        GetKeyState(key.0 as i32) < 0
    };
    let modifiers = Modifiers {
        control: down(VK_CONTROL),
        alt: down(VK_MENU),
        shift: down(VK_SHIFT),
        platform: down(VK_LWIN) || down(VK_RWIN),
        function: false,
    };
    let key = key_name_for_vk(vk)?;
    let is_function_key = key.starts_with('f') && key.len() > 1 && key[1..].parse::<u8>().is_ok();
    if !(modifiers.control || modifiers.alt || is_function_key) {
        return None;
    }
    Some((modifiers, key))
}

fn key_name_for_vk(vk: u32) -> Option<String> {
    let name = match vk {
        0x08 => "backspace",
        0x09 => "tab",
        0x0D => "enter",
        0x1B => "escape",
        0x20 => "space",
        0x21 => "pageup",
        0x22 => "pagedown",
        0x23 => "end",
        0x24 => "home",
        0x25 => "left",
        0x26 => "up",
        0x27 => "right",
        0x28 => "down",
        0x2D => "insert",
        0x2E => "delete",
        0x70..=0x87 => return Some(format!("f{}", vk - 0x70 + 1)),
        _ => {
            let char_code = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR) } & 0xFFFF;
            let character = char::from_u32(char_code).filter(|c| !c.is_control())?;
            return Some(character.to_ascii_lowercase().to_string());
        }
    };
    Some(name.to_string())
}

trait LogFailure {
    fn log_failure(self, what: &str);
}

impl LogFailure for windows_core::Result<()> {
    fn log_failure(self, what: &str) {
        if let Err(error) = self {
            log::warn!("WebView2 {what}: {error}");
        }
    }
}

#[cfg(test)]
mod tests;
