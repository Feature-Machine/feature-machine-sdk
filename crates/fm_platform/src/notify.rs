// OS notifications of the app (game policies plan, slice 3): a Windows toast for a server notice
// that arrives while the app is in the background. An app with no package shows toasts only for
// an app id that the registry names (HKCU\Software\Classes\AppUserModelId\<id>, the Windows App
// SDK way): `register` writes it at setup and at each app start (installs from before this
// version), `unregister` removes it at uninstall. Other OSes: no notification yet.
use std::io;
use std::path::Path;

/// The app id of the toasts: the app id of the data folders, which is the Tauri identifier (the
/// app test tauri_identifier_equals_app_id).
use crate::APP_ID;

/// One toast. `launch`: the featuremachine:// link that a click opens (it starts or shows the app).
pub struct Toast<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub launch: &'a str,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// The toast XML (ToastGeneric: a title line and a text line). A click opens `launch` as a link.
pub fn toast_xml(t: &Toast) -> String {
    format!(
        "<toast launch=\"{}\" activationType=\"protocol\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape(t.launch),
        escape(t.title),
        escape(t.body)
    )
}

#[cfg(windows)]
fn key() -> String {
    format!(r"Software\Classes\AppUserModelId\{APP_ID}")
}

/// Names the app id in the registry with the product name and its icon (a PNG file).
#[cfg(windows)]
pub fn register(icon: Option<&Path>) -> io::Result<()> {
    let (k, _) = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).create_subkey(key())?;
    k.set_value("DisplayName", &crate::install::PRODUCT)?;
    if let Some(i) = icon {
        k.set_value("IconUri", &i.display().to_string())?;
    }
    Ok(())
}

/// Removes the app id. Already gone is not an error.
#[cfg(windows)]
pub fn unregister() -> io::Result<()> {
    match winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).delete_subkey_all(key()) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Shows the toast. WinRT runs on its own thread in the multithreaded COM apartment, so the
/// caller's thread (a tokio worker) keeps its COM state.
#[cfg(windows)]
pub fn show(t: &Toast) -> io::Result<()> {
    let xml = toast_xml(t);
    std::thread::spawn(move || unsafe {
        use windows::core::HSTRING;
        use windows::Data::Xml::Dom::XmlDocument;
        use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
        let init = CoInitializeEx(None, COINIT_MULTITHREADED);
        let result = (|| {
            let doc = XmlDocument::new()?;
            doc.LoadXml(&HSTRING::from(xml))?;
            let toast = ToastNotification::CreateToastNotification(&doc)?;
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result.map_err(|e| io::Error::other(e.to_string()))
    })
    .join()
    .map_err(|_| io::Error::other("the toast thread stopped"))?
}

#[cfg(not(windows))]
pub fn register(_icon: Option<&Path>) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn unregister() -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn show(_t: &Toast) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no OS notifications on this system"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_text_is_escaped() {
        let x = toast_xml(&Toast { title: "A <b> & \"c\"", body: "it's", launch: "featuremachine://notices/a-b" });
        assert_eq!(x, "<toast launch=\"featuremachine://notices/a-b\" activationType=\"protocol\"><visual><binding template=\"ToastGeneric\"><text>A &lt;b&gt; &amp; &quot;c&quot;</text><text>it&apos;s</text></binding></visual></toast>");
    }
}
