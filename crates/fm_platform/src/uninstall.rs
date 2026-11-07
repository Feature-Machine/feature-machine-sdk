// Reviewed uninstall registrations identify ordinary Windows applications. Source keys, not
// paths or versions, retain member setup when a vendor updates or moves its installation.
use crate::applications::{ApplicationInstall, ApplicationInstance, ApplicationService};
#[cfg(any(windows, test))]
use crate::applications::{InstanceKind, Source};
#[cfg(any(windows, test))]
use std::path::{Path, PathBuf};

#[cfg(test)]
const SERVICE: &str = "DboxHaptiSyncApi";
#[cfg(test)]
const UI_EXE: &str = "HaptiSyncCenter.App.exe";
#[cfg(test)]
const API_EXE: &str = "HaptiSyncApi.WebApi.exe";

#[cfg(any(windows, test))]
#[derive(Clone, Debug)]
struct Registration { key: String, name: String, publisher: String, location: PathBuf, version: Option<String> }

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub fn installs() -> Vec<ApplicationInstall> {
    let mut installs = Vec::new();
    for definition in fm_application::definitions() {
        let Some(rule) = &definition.windows_uninstall else { continue };
        for registration in registrations().iter().filter(|r| r.name == rule.display_name) {
            if let Some(install) = resolve(registration, definition, &|p| vendor_image(p, &rule.publisher), &|| configured_service(&rule.service, &rule.publisher)) { installs.push(install); }
        }
    }
    reject_ambiguous(&mut installs);
    installs
}

#[cfg(not(windows))]
pub fn installs() -> Vec<ApplicationInstall> { Vec::new() }

/// An unavailable registration stays visible with its stable identity and reason. A registry
/// string does not make a file trusted; exactly one signed reviewed UI image must resolve.
#[cfg(any(windows, test))]
fn resolve(reg: &Registration, definition: &fm_application::ApplicationDefinition, signature: &dyn Fn(&Path) -> Result<(), String>, service: &dyn Fn() -> Result<ApplicationService, String>) -> Option<ApplicationInstall> {
    let rule = definition.windows_uninstall.as_ref()?;
    if reg.name != rule.display_name || !reg.publisher.eq_ignore_ascii_case(&rule.publisher) { return None; }
    let preferred = reg.location.join(rule.executables.first()?);
    let candidates: Vec<_> = rule.executables.iter().map(|rel| reg.location.join(rel)).filter(|p| p.is_file()).collect();
    let valid: Vec<_> = candidates.iter().filter(|p| signature(p).is_ok()).cloned().collect();
    let (exe, mut error) = match valid.as_slice() {
        [one] => (one.clone(), None),
        [] => (candidates.first().cloned().unwrap_or(preferred.clone()), Some(format!("The registered {} image is missing or has an invalid publisher signature.", definition.name))),
        _ => (preferred, Some(format!("The registered {} location has more than one valid image.", definition.name))),
    };
    let api = if error.is_none() { match service() { Ok(s) => Some(s), Err(e) => { error = Some(e); None } } } else { None };
    Some(ApplicationInstall { game: definition.id, source: Source::WindowsUninstall,
        source_key: Some(reg.key.clone()), install_dir: exe.parent().unwrap_or(&reg.location).into(),
        exe, version: reg.version.clone(), steam_build_id: None, ready: error.is_none(),
        running_pid: None, instances: Vec::new(), service: api, error })
}

/// SCM may store a quoted path or an unquoted path with spaces. No executable search is
/// performed: only an absolute image path ending at .exe can enter identity verification.
#[cfg(windows)]
fn service_executable(command: &str) -> Result<PathBuf, String> {
    let text = command.trim();
    let image = if let Some(t) = text.strip_prefix('"') { t.split_once('"').ok_or("unterminated service image path")?.0 }
        else { let n = text.to_ascii_lowercase().find(".exe").ok_or("service image is not an executable")? + 4; &text[..n] };
    let p = PathBuf::from(image);
    if p.is_absolute() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) { Ok(p) }
    else { Err("service image must be an absolute executable path".into()) }
}

#[cfg(windows)]
fn vendor_image(path: &Path, expected_publisher: &str) -> Result<(), String> {
    let publisher = windows::signer(path)?;
    if publisher.eq_ignore_ascii_case(expected_publisher) { Ok(()) } else { Err(format!("unexpected image publisher: {publisher}")) }
}

#[cfg(windows)]
fn configured_service(rule: &fm_application::ServiceDefinition, publisher: &str) -> Result<ApplicationService, String> {
    let service = windows::service(&rule.name)?;
    if !service.exe.file_name().is_some_and(|n| n.eq_ignore_ascii_case(&rule.executable)) { return Err(format!("The {} service has an unexpected executable.", rule.name)); }
    vendor_image(&service.exe, publisher)?;
    Ok(ApplicationService { name: rule.name.clone(), exe: service.exe })
}

/// Connection admission rechecks SCM, the signed image and its process start identity. The
/// HTTP destination has separate loopback-owner proof, rather than trusting a port number.
#[cfg(windows)]
fn service_instance(expected: &ApplicationService, listener: Option<u16>) -> Result<Option<ApplicationInstance>, String> {
    let rule = fm_application::definitions().iter().filter_map(|d| d.windows_uninstall.as_ref()).find(|r| r.service.name == expected.name).ok_or("service has no reviewed connector")?;
    let configured = configured_service(&rule.service, &rule.publisher)?;
    if !crate::applications::same_path(&configured.exe, &expected.exe) { return Err("the service image changed".into()); }
    let Some(pid) = windows::service(&rule.service.name)?.pid else { return Ok(None); };
    let (exe, start_id) = windows::process(pid).map_err(|e| e.to_string())?;
    if start_id == 0 || !crate::applications::same_path(&exe, &expected.exe) { return Err("the service process does not match its signed configuration".into()); }
    if let Some(port) = listener { windows::listener(port, pid)?; }
    Ok(Some(ApplicationInstance { kind: InstanceKind::Service, pid, start_id, exe }))
}

#[cfg(not(windows))]
fn service_instance(_: &ApplicationService, _: Option<u16>) -> Result<Option<ApplicationInstance>, String> { Err("Windows service discovery is unavailable".into()) }

/// A stopped service is a valid observation but cannot admit a runtime connection. Keep it
/// distinct from an observation error so discovery can report unavailable identity proof.
pub fn verify_service(expected: &ApplicationService, listener: Option<u16>) -> Result<ApplicationInstance, String> {
    service_instance(expected, listener)?.ok_or_else(|| "the application service is stopped".into())
}

/// Service observations never populate running_pid, which remains the desktop UI projection.
/// A failed observation removes the previous instance, so a restart cannot retain stale data.
pub fn observe(installs: &mut [ApplicationInstall]) {
    observe_with(installs, &|s| service_instance(s, None));
}

fn observe_with(installs: &mut [ApplicationInstall], inspect: &dyn Fn(&ApplicationService) -> Result<Option<ApplicationInstance>, String>) {
    for install in installs {
        install.instances.retain(|i| i.kind != crate::applications::InstanceKind::Service);
        if !install.ready { continue; }
        if let Some(service) = &install.service {
            match inspect(service) {
                Ok(instance) => { install.error = None; install.instances.extend(instance); }
                Err(error) => install.error = Some(error),
            }
        }
    }
}

// HKLM has separate 64-bit and 32-bit Uninstall keys. HKCU\Software is shared under WOW64, so
// a second HKCU view gives each per-user key twice with two install ids (review B2). The HKCU
// label stays "64" so the id of a per-user install found before this change does not move.
#[cfg(windows)]
const VIEWS: [(&str, winreg::HKEY, &str, u32); 3] = [
    ("hklm", winreg::enums::HKEY_LOCAL_MACHINE, "64", winreg::enums::KEY_WOW64_64KEY),
    ("hklm", winreg::enums::HKEY_LOCAL_MACHINE, "32", winreg::enums::KEY_WOW64_32KEY),
    ("hkcu", winreg::enums::HKEY_CURRENT_USER, "64", 0),
];

#[cfg(windows)]
fn registrations() -> Vec<Registration> {
    use winreg::{enums::*, RegKey};
    let mut out = Vec::new();
    for (hive, root, view, flag) in VIEWS {
        let Ok(uninstall) = RegKey::predef(root).open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", KEY_READ | flag) else { continue };
        for key in uninstall.enum_keys().filter_map(Result::ok) {
            let Ok(entry) = uninstall.open_subkey_with_flags(&key, KEY_READ | flag) else { continue };
            let Ok(name) = entry.get_value::<String, _>("DisplayName") else { continue };
            if !fm_application::definitions().iter().filter_map(|d| d.windows_uninstall.as_ref()).any(|r| r.display_name == name) { continue; }
            let Ok(publisher) = entry.get_value::<String, _>("Publisher") else { continue };
            let Ok(location) = entry.get_value::<String, _>("InstallLocation") else { continue };
            if !Path::new(&location).is_absolute() { continue; }
            out.push(Registration { key: format!("{hive}/{view}/{}", key.to_lowercase()), name, publisher,
                location: location.into(), version: entry.get_value("DisplayVersion").ok() });
        }
    }
    out
}

// Two registry entries must not claim one service instance. Keep both install identities,
// but require an unambiguous registration before admitting either connection.
#[cfg(any(windows, test))]
fn reject_ambiguous(installs: &mut [ApplicationInstall]) {
    let duplicate: Vec<_> = installs.iter().enumerate().filter_map(|(n, i)| i.service.as_ref().filter(|s| installs.iter().enumerate().any(|(m, other)| m != n && other.service.as_ref().is_some_and(|o| o.name == s.name))).map(|_| n)).collect();
    for n in duplicate { installs[n].ready = false; installs[n].error = Some("More than one registered install claims this service.".into()); }
}

#[cfg(test)]
mod tests;
