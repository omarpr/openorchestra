//! Agent notifications on Windows: WinRT toasts.
//!
//! **Being a known app.** Windows accepts any app's toasts into its history,
//! which looks like success, but shows a banner only for an unpackaged app
//! whose AppUserModelID is on a Start menu shortcut. So on start the app
//! makes sure there is one (`openOrchestra.lnk` in the user's Start menu,
//! pointing at this exe when it makes it), carrying the id and the CLSID of
//! its toast activator; registers the id's display name and icon under
//! `HKCU\Software\Classes\AppUserModelId`; and sets it as the process's
//! explicit id (which also groups its windows on the taskbar).
//!
//! **Clicks.** Toast clicks reach an unpackaged app through COM, not the
//! toast's in-process `Activated` event (which never fires for one): the
//! toast thread registers an `INotificationActivationCallback` class object
//! under the shortcut's CLSID. **View** (or the banner body) selects the
//! alert's worktree and agent tab, as on macOS; **Dismiss** is the system's
//! own. `HKCU\Software\Classes\CLSID\{…}\LocalServer32` names this exe, so a
//! click on an old toast in Action Center after the app quit starts it
//! (with `-ToastActivated`, which `main` ignores).
//!
//! **Threading.** One thread owns the toasts, in the multithreaded
//! apartment: posting is a cross-apartment WinRT call that pumps messages
//! while it waits, which from gpui's thread re-entered gpui's window
//! procedure mid-update, and an MTA activator is called on COM's own
//! threads with no message loop to pump.
//!
//! **Sound.** Toasts are posted silent and the app plays the chosen sound
//! itself (`sounds::play`), so the bundled sounds work; the system default
//! keeps the toast's own.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::OnceLock;

use crossbeam_channel::Sender;
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::Win32::Foundation::CLASS_E_NOAGGREGATION;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoRegisterClassObject, IClassFactory, IClassFactory_Impl, IPersistFile,
    REGCLS_MULTIPLEUSE, STGM_READWRITE,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW,
};
use windows::Win32::UI::Notifications::{
    INotificationActivationCallback, INotificationActivationCallback_Impl,
    NOTIFICATION_USER_INPUT_DATA,
};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::core::{BOOL, GUID, HSTRING, IUnknown, Interface, PCWSTR, Ref, implement};

pub const AUMID: &str = "openOrchestra.Harness";
/// The toast activator's CLSID (ours; on the shortcut and in the registry).
const ACTIVATOR: GUID = GUID::from_u128(0xc6d5773d_967c_4327_bee3_fa480725b2c4);
const VIEW_PREFIX: &str = "view:";

static NAV: OnceLock<Sender<String>> = OnceLock::new();
/// The toast thread's queue: title, body, worktree id, sound id.
type Post = (String, String, String, String);
static QUEUE: OnceLock<Sender<Post>> = OnceLock::new();

/// Sets the app id (before any window exists) and starts the toast thread.
pub fn init(nav: Sender<String>) {
    let _ = NAV.set(nav);
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(AUMID));
    }
    let (tx, rx) = crossbeam_channel::unbounded::<Post>();
    if QUEUE.set(tx).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("toasts".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        if let Err(e) = register_app() {
            log::warn!("notifications: could not register the app id: {e}");
        }
        if let Err(e) = ensure_shortcut() {
            log::warn!("notifications: no Start menu shortcut: {e}");
        }
        // Kept for the life of the thread, which is the app's.
        let _activator = match unsafe {
            CoRegisterClassObject(
                &ACTIVATOR,
                &IClassFactory::from(Factory),
                CLSCTX_LOCAL_SERVER,
                REGCLS_MULTIPLEUSE,
            )
        } {
            Ok(cookie) => Some(cookie),
            Err(e) => {
                log::warn!("notifications: clicks won't reach the app: {e}");
                None
            }
        };
        let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID)) {
            Ok(n) => n,
            Err(e) => {
                log::warn!("notifications: no toast notifier: {e}");
                return;
            }
        };
        // Recent toasts stay referenced while their banners plausibly show.
        let mut recent: VecDeque<ToastNotification> = VecDeque::new();
        for (title, body, worktree_id, sound) in rx {
            if let Some(toast) = show(&notifier, &title, &body, &worktree_id, &sound) {
                recent.push_back(toast);
                while recent.len() > 20 {
                    recent.pop_front();
                }
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("notifications: no toast thread: {e}");
    }
}

/// Queues a toast for the toast thread; `false` when there is none.
pub fn post(title: &str, body: &str, worktree_id: &str, sound: &str) -> bool {
    QUEUE
        .get()
        .is_some_and(|q| q.send((title.into(), body.into(), worktree_id.into(), sound.into())).is_ok())
}

/// What a click hands the activator: the toast's or the button's
/// `arguments`.
fn activated(arguments: &str) {
    log::info!("notifications: toast activated ({arguments})");
    if let Some(worktree) = arguments.strip_prefix(VIEW_PREFIX)
        && let Some(nav) = NAV.get()
    {
        let _ = nav.send(worktree.to_string());
    }
}

#[implement(INotificationActivationCallback)]
struct Activator;

impl INotificationActivationCallback_Impl for Activator_Impl {
    fn Activate(
        &self,
        _app: &PCWSTR,
        arguments: &PCWSTR,
        _data: *const NOTIFICATION_USER_INPUT_DATA,
        _count: u32,
    ) -> windows::core::Result<()> {
        let arguments = if arguments.is_null() {
            String::new()
        } else {
            unsafe { arguments.to_string() }.unwrap_or_default()
        };
        activated(&arguments);
        Ok(())
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activator: IUnknown = Activator.into();
        unsafe { activator.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Writes a `REG_SZ` value (`None`: the key's default) under HKCU.
fn set_registry(key_path: &str, name: Option<&str>, value: &str) -> windows::core::Result<()> {
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key_path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        let name = name.map(HSTRING::from);
        let result = match &name {
            Some(n) => RegSetValueExW(key, n, None, REG_SZ, Some(bytes)),
            None => RegSetValueExW(key, PCWSTR::null(), None, REG_SZ, Some(bytes)),
        }
        .ok();
        let _ = RegCloseKey(key);
        result
    }
}

/// The id's display name and icon (`HKCU\Software\Classes\AppUserModelId`).
/// The icon is written next to the app's other per-user data, since a dev
/// binary has no install folder.
fn register_app() -> windows::core::Result<()> {
    let icon = std::env::var_os("LOCALAPPDATA")
        .map(|d| std::path::PathBuf::from(d).join("openOrchestra").join("notification-icon.png"));
    if let Some(icon) = &icon
        && !icon.exists()
    {
        if let Some(dir) = icon.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(icon, include_bytes!("../../../web/remote/public/icon-192.png"));
    }
    let key = format!("Software\\Classes\\AppUserModelId\\{AUMID}");
    set_registry(&key, Some("DisplayName"), "openOrchestra")?;
    if let Some(icon) = &icon {
        set_registry(&key, Some("IconUri"), &icon.to_string_lossy())?;
    }
    Ok(())
}

/// The Start menu shortcut the banner needs (see the module docs), with the
/// app id and the activator's CLSID. An existing one keeps its target (the
/// installer's); one made here points at this exe, and so does the
/// activator's `LocalServer32` then.
fn ensure_shortcut() -> windows::core::Result<()> {
    use windows::Win32::Storage::EnhancedStorage::{
        PKEY_AppUserModel_ID, PKEY_AppUserModel_ToastActivatorCLSID,
    };
    use windows::Win32::System::Com::CoTaskMemAlloc;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Variant::{VT_CLSID, VT_LPWSTR};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, SHStrDupW, ShellLink};

    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Ok(());
    };
    let lnk = std::path::PathBuf::from(appdata)
        .join("Microsoft\\Windows\\Start Menu\\Programs\\openOrchestra.lnk");
    let exe = std::env::current_exe()
        .map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e.to_string()))?;
    let lnk_w = HSTRING::from(lnk.as_os_str());
    let existed = lnk.exists();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        if existed {
            file.Load(&lnk_w, STGM_READWRITE)?;
        } else {
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            link.SetDescription(&HSTRING::from("Run coding agents in parallel git worktrees"))?;
        }
        let store: IPropertyStore = link.cast()?;
        // Both values are CoTaskMem allocations: PROPVARIANT's Drop frees
        // them (PropVariantClear).
        let mut id = PROPVARIANT::default();
        (*id.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*id.Anonymous.Anonymous).Anonymous.pwszVal = SHStrDupW(&HSTRING::from(AUMID))?;
        let guid = CoTaskMemAlloc(std::mem::size_of::<GUID>()) as *mut GUID;
        if guid.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        guid.write(ACTIVATOR);
        let mut clsid = PROPVARIANT::default();
        (*clsid.Anonymous.Anonymous).vt = VT_CLSID;
        (*clsid.Anonymous.Anonymous).Anonymous.puuid = guid;
        store.SetValue(&PKEY_AppUserModel_ID, &id)?;
        store.SetValue(&PKEY_AppUserModel_ToastActivatorCLSID, &clsid)?;
        store.Commit()?;
        file.Save(&lnk_w, true)?;
    }
    if !existed {
        log::info!("notifications: added the Start menu shortcut {}", lnk.display());
    }
    // COM finds the activator through this registration, even while the
    // running app has its class object registered; it names the
    // shortcut's exe (this one, unless an installed shortcut points
    // elsewhere, whose own run registers it).
    if !existed || shortcut_target(&lnk).is_some_and(|t| same_file(&t, &exe)) {
        let server = format!("Software\\Classes\\CLSID\\{{{ACTIVATOR:?}}}\\LocalServer32");
        set_registry(&server, None, &format!("\"{}\" -ToastActivated", exe.display()))?;
    }
    Ok(())
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Where an existing shortcut points.
fn shortcut_target(lnk: &std::path::Path) -> Option<std::path::PathBuf> {
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(lnk.as_os_str()), windows::Win32::System::Com::STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The toast's XML: title, body, View and Dismiss. `silent`: the app plays
/// the sound itself.
fn toast_xml(title: &str, body: &str, worktree_id: &str, silent: bool) -> String {
    let view = xml_escape(&format!("{VIEW_PREFIX}{worktree_id}"));
    let audio = if silent { r#"<audio silent="true"/>"# } else { "" };
    format!(
        r#"<toast launch="{view}" activationType="foreground"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual><actions><action content="View" arguments="{view}" activationType="foreground"/><action content="Dismiss" arguments="dismiss" activationType="system"/></actions>{audio}</toast>"#,
        xml_escape(title),
        xml_escape(body),
    )
}

/// On the toast thread: shows one, and plays its sound.
fn show(
    notifier: &ToastNotifier,
    title: &str,
    body: &str,
    worktree_id: &str,
    sound: &str,
) -> Option<ToastNotification> {
    use crate::sounds::{self, Source};
    let chosen = sounds::resolve(sound);
    let silent = chosen.source != Source::SystemDefault;
    let shown = (|| -> windows::core::Result<ToastNotification> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(toast_xml(title, body, worktree_id, silent)))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        notifier.Show(&toast)?;
        Ok(toast)
    })();
    match shown {
        Ok(toast) => {
            if silent {
                sounds::play(chosen);
            }
            Some(toast)
        }
        Err(e) => {
            log::warn!("notifications: Windows rejected the toast: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::toast_xml;

    #[test]
    fn toast_text_is_xml_escaped() {
        // A branch called `a<b&"c"` must not break (or inject into) the XML.
        let xml = toast_xml(r#"a<b&"c""#, "x>y", "wt'1", true);
        assert!(xml.contains("<text>a&lt;b&amp;&quot;c&quot;</text>"), "{xml}");
        assert!(xml.contains("<text>x&gt;y</text>"), "{xml}");
        assert!(xml.contains(r#"arguments="view:wt&apos;1""#), "{xml}");
        assert!(xml.contains(r#"<audio silent="true"/>"#));
        assert!(!toast_xml("t", "b", "w", false).contains("<audio"));
    }
}
