// Registry fixtures exercise identity and location rules without reading member software.
// The injected signature/service evidence represents the two native read-only boundaries.
use super::*;
use crate::ApplicationId;
const PUBLISHER: &str = "D-BOX Technologies Inc.";
fn definition() -> &'static fm_application::ApplicationDefinition { ApplicationId::DBOX_HAPTISYNC_CENTER.definition().unwrap() }

fn fixture(root: &Path) -> Registration {
    Registration { key: "hklm/64/d-box haptisync center".into(), name: "D-BOX HaptiSync Center".into(),
        publisher: PUBLISHER.into(), location: root.into(), version: Some("1.4.1".into()) }
}

#[test]
fn registered_identity_survives_location_and_version_changes() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("vendor-a");
    let b = temp.path().join("vendor-b");
    for root in [&a, &b] {
        std::fs::create_dir_all(root.join("HaptiSync Center")).unwrap();
        std::fs::write(root.join("HaptiSync Center").join(UI_EXE), b"signed fixture").unwrap();
    }
    let signature = |_: &Path| Ok(());
    let service = || Ok(ApplicationService { name: SERVICE.into(), exe: temp.path().join(API_EXE) });
    let first = resolve(&fixture(&a), definition(), &signature, &service).unwrap();
    let mut registration = fixture(&b);
    registration.version = Some("2.0.0".into());
    let moved = resolve(&registration, definition(), &signature, &service).unwrap();
    assert_eq!(first.id(), moved.id());
    assert!(first.id().len() <= 64);
    assert_ne!(first.install_dir, moved.install_dir);
    assert_eq!(first.exe, a.join("HaptiSync Center").join(UI_EXE));
    assert_eq!(first.source, Source::WindowsUninstall);
    assert!(first.ready && moved.ready);
    assert!(first.running_pid.is_none() && first.instances.is_empty());
    assert_eq!(first.service.unwrap().exe, temp.path().join(API_EXE));
}

#[test]
fn wrong_or_missing_binaries_never_become_ready() {
    let temp = tempfile::tempdir().unwrap();
    let reg = fixture(temp.path());
    let service = || Ok(ApplicationService { name: SERVICE.into(), exe: temp.path().join(API_EXE) });
    let signature = |_: &Path| Err("wrong signer".to_string());
    let missing = resolve(&reg, definition(), &signature, &service).unwrap();
    assert!(!missing.ready && missing.error.is_some());
    std::fs::write(temp.path().join(UI_EXE), b"wrong fixture").unwrap();
    let wrong = resolve(&reg, definition(), &signature, &service).unwrap();
    assert!(!wrong.ready && wrong.instances.is_empty());
    let good_signature = |_: &Path| Ok(());
    let bad_service = || Err("wrong service image".to_string());
    let wrong = resolve(&reg, definition(), &good_signature, &bad_service).unwrap();
    assert!(!wrong.ready && wrong.service.is_none());
    let mut foreign = reg;
    foreign.publisher = "Other publisher".into();
    assert!(resolve(&foreign, definition(), &good_signature, &service).is_none());
}

#[test]
fn ui_and_service_observations_do_not_share_a_lifetime() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(UI_EXE), b"fixture").unwrap();
    let mut install = resolve(&fixture(temp.path()), definition(), &|_| Ok(()), &|| Ok(ApplicationService { name: SERVICE.into(), exe: temp.path().join(API_EXE) })).unwrap();
    let service = ApplicationInstance { kind: InstanceKind::Service, pid: 91, start_id: 1001, exe: temp.path().join(API_EXE) };
    crate::applications::mark_running(std::slice::from_mut(&mut install), &[]);
    observe_with(std::slice::from_mut(&mut install), &|_| Ok(Some(service.clone())));
    assert!(install.running_pid.is_none(), "a service is not an open Center UI");
    assert_eq!(install.instances.as_slice(), std::slice::from_ref(&service));
    let mut restart = service;
    restart.start_id += 1;
    observe_with(std::slice::from_mut(&mut install), &|_| Ok(Some(restart.clone())));
    assert_eq!(install.instances, [restart]);
    observe_with(std::slice::from_mut(&mut install), &|_| Ok(None));
    assert!(install.instances.is_empty(), "a failed observation removes the old service instance");
    assert!(install.ready, "a stopped service does not remove its install");
    assert!(install.error.is_none(), "a verified stopped service is not an observation error");
}

#[test]
fn an_unverified_service_reports_its_error_and_a_recovery_clears_it() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(UI_EXE), b"fixture").unwrap();
    let mut install = resolve(&fixture(temp.path()), definition(), &|_| Ok(()), &|| Ok(ApplicationService { name: SERVICE.into(), exe: temp.path().join(API_EXE) })).unwrap();
    observe_with(std::slice::from_mut(&mut install), &|_| Err("Access is denied. (os error 5)".into()));
    assert!(install.error.as_deref().is_some_and(|e| e.contains("Access is denied")));
    assert!(install.ready && install.instances.is_empty());
    let service = ApplicationInstance { kind: InstanceKind::Service, pid: 91, start_id: 1001, exe: temp.path().join(API_EXE) };
    observe_with(std::slice::from_mut(&mut install), &|_| Ok(Some(service.clone())));
    assert_eq!(install.instances, [service]);
    assert!(install.error.is_none(), "the next verified observation clears the old error");
}

#[cfg(windows)]
#[test]
fn quoted_and_unquoted_service_commands_resolve_only_absolute_images() {
    assert_eq!(service_executable(r#""C:\Program Files\D-BOX\HaptiSync Api\HaptiSyncApi.WebApi.exe" --service"#).unwrap(), PathBuf::from(r"C:\Program Files\D-BOX\HaptiSync Api\HaptiSyncApi.WebApi.exe"));
    assert_eq!(service_executable(r"C:\Program Files\D-BOX\HaptiSync Api\HaptiSyncApi.WebApi.exe --service").unwrap(), PathBuf::from(r"C:\Program Files\D-BOX\HaptiSync Api\HaptiSyncApi.WebApi.exe"));
    assert!(service_executable("relative.exe --service").is_err());
    assert!(service_executable(r#""C:\wrong.bat""#).is_err());
    assert!(service_executable(r#""C:\broken.exe"#).is_err());
}

#[test]
fn duplicate_registrations_cannot_claim_one_service() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(UI_EXE), b"fixture").unwrap();
    let first = resolve(&fixture(temp.path()), definition(), &|_| Ok(()), &|| Ok(ApplicationService { name: SERVICE.into(), exe: temp.path().join(API_EXE) })).unwrap();
    let mut second = first.clone();
    second.source_key = Some("hkcu/64/another-key".into());
    let mut installs = vec![first, second];
    reject_ambiguous(&mut installs);
    assert!(installs.iter().all(|i| !i.ready && i.error.is_some()));
    assert_ne!(installs[0].id(), installs[1].id());
}

#[test]
fn desktop_observation_requires_the_registered_image_path() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(UI_EXE), b"fixture").unwrap();
    let mut install = resolve(&fixture(temp.path()), definition(), &|_| Ok(()), &|| Ok(ApplicationService { name: SERVICE.into(), exe: temp.path().join(API_EXE) })).unwrap();
    let mut process = crate::process::ProcInfo { pid: 10, start_id: 90, name: UI_EXE.into(), exe: None, cmd: vec![] };
    crate::applications::mark_running(std::slice::from_mut(&mut install), &[process.clone()]);
    assert!(install.running_pid.is_none(), "a hidden image path is not vendor identity");
    process.exe = Some(install.exe.clone());
    crate::applications::mark_running(std::slice::from_mut(&mut install), &[process]);
    assert_eq!(install.running_pid, Some(10));
    assert_eq!(install.instances[0].kind, InstanceKind::Desktop);
    assert_eq!(install.instances[0].start_id, 90);
    let serialized = serde_json::to_value(&install).unwrap();
    assert_eq!(serialized["instances"][0]["start_id"], "90");
}

// Review B2: HKCU\Software is shared under WOW64. The read-only check proves that premise on
// this machine; the view list must then read HKCU once, or each per-user key appears twice.
#[cfg(windows)]
#[test]
fn each_registry_view_is_read_once() {
    use winreg::{enums::*, RegKey};
    let keys = |flag| RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", KEY_READ | flag)
        .map(|k| k.enum_keys().filter_map(Result::ok).collect::<Vec<_>>()).ok();
    assert_eq!(keys(KEY_WOW64_64KEY), keys(KEY_WOW64_32KEY), "HKCU Uninstall is one key in both views");
    let views: Vec<_> = VIEWS.iter().map(|(hive, _, view, _)| format!("{hive}/{view}")).collect();
    assert_eq!(views, ["hklm/64", "hklm/32", "hkcu/64"]);
}
