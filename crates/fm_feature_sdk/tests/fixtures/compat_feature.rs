// Frozen API 1 to 6 library fixture. It uses a fixed ABI copy so a changed SDK cannot
// silently change the old caller and make a compatibility test pass for the wrong reason.
#[path = "abi_2026_11_06.rs"]
#[allow(dead_code)]
mod abi;
use abi::*;
use std::ffi::c_void;

#[no_mangle]
pub extern "C" fn fm_feature_api_version() -> u32 {
    env!("FM_FIXTURE_API").parse().unwrap()
}

unsafe fn log(instance: *mut c_void, text: &str) {
    let host = &*instance.cast::<HostApi>();
    (host.log)(host.ctx, text.as_ptr(), text.len());
}

unsafe extern "C" fn frame(instance: *mut c_void, _: *const AbiFrame) -> i32 {
    log(instance, "fixture:frame");
    OK
}

unsafe extern "C" fn event(instance: *mut c_void, _: *const u8, _: usize) -> i32 {
    log(instance, "fixture:event");
    OK
}

unsafe extern "C" fn http(_: *mut c_void, _: u64, _: i32, _: *const u8, _: usize) -> i32 { OK }
unsafe extern "C" fn application_event(instance: *mut c_void, _: *const AbiEvent) -> i32 {
    log(instance, "fixture:event");
    OK
}
unsafe extern "C" fn stop(instance: *mut c_void) { log(instance, "fixture:stop"); }

#[no_mangle]
pub unsafe extern "C" fn fm_feature_start(host: *const HostApi, out: *mut FeatureApi) -> i32 {
    let version = fm_feature_api_version();
    if host.is_null() || out.is_null() || (*host).api_version != version { return BAD_ARGS; }
    if text((*host).game, (*host).game_len) != Some("ats") { return BAD_ARGS; }
    if version >= 6 && text((*host).settings, (*host).settings_len) != Some("{}") { return BAD_ARGS; }
    let instance = host.cast_mut().cast();
    log(instance, "fixture:start");
    let table = FeatureApi {
        api_version: version, instance, on_frame: Some(frame), on_event: Some(event),
        stop: Some(stop), on_http: Some(http), on_game_event: Some(application_event),
    };
    // Older libraries write only their original table. The host must supply the absent tail.
    let size = match version {
        1 | 2 => std::mem::offset_of!(FeatureApi, on_http),
        3 => std::mem::offset_of!(FeatureApi, on_game_event),
        _ => std::mem::size_of::<FeatureApi>(),
    };
    std::ptr::copy_nonoverlapping((&table as *const FeatureApi).cast::<u8>(), out.cast::<u8>(), size);
    OK
}
