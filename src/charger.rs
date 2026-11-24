





#![allow(non_snake_case)]

use std::fs;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn hook_log(_msg: &str) {}

use rsbinder::{BinderResult, Interface};

use crate::aidl_bindings::vendor::oplus::hardware::charger::ChgFuncFlag::ChgFuncFlag;
use crate::aidl_bindings::vendor::oplus::hardware::charger::ICharger::ICharger;
use crate::aidl_bindings::vendor::oplus::hardware::charger::testKitFeatureTestResult::testKitFeatureTestResult;


const DECIMAL_FAST_WINDOW: f64 = 60.0;
const DECIMAL_FAST_INTERVAL: f64 = 0.20;
const DECIMAL_CHARGING_INTERVAL: f64 = 0.50;
const DECIMAL_IDLE_INTERVAL: f64 = 1.0;
const DECIMAL_WRAP_HOLD_SECS: f64 = 5.0;

const SOC_DECIMAL_PATHS: &[&str] = &[
    "/sys/class/qcom-battery/soc_decimal",
    "/sys/class/power_supply/bms/soc_decimal",
    "/sys/class/power_supply/battery/soc_decimal",
];






const CHARGE_END_THRESHOLD_PATHS: &[&str] = &[
    "/sys/class/power_supply/battery/charge_control_end_threshold",
    "/sys/class/qcom-battery/charge_control_end_threshold",
];
const CHARGE_START_THRESHOLD_PATHS: &[&str] = &[
    "/sys/class/power_supply/battery/charge_control_start_threshold",
    "/sys/class/qcom-battery/charge_control_start_threshold",
];
const CHARGE_INPUT_SUSPEND_PATHS: &[&str] = &[
    "/sys/class/qcom-battery/input_suspend",
];
const CHARGE_LIMIT_RESUME_DELTA: i32 = 5;

const RESUME_COOLDOWN_SECS: u64 = 60;


fn read_raw(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}



fn read_long(path: &str) -> Option<i64> {
    let s = read_raw(path)?;
    let t = s.trim_start();
    let bytes = t.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let start_digits = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == start_digits {
        return None;
    }
    t[..i].parse::<i64>().ok()
}


fn read_int_node(path: &str) -> i32 {
    read_long(path).map(|v| v as i32).unwrap_or(-1)
}


fn read_milli_node(path: &str, fallback: i32) -> i32 {
    read_long(path).map(|v| (v / 1000) as i32).unwrap_or(fallback)
}


fn read_int_or(path: &str, fallback: i32) -> i32 {
    read_long(path).map(|v| v as i32).unwrap_or(fallback)
}


fn read_str_node(path: &str) -> String {
    match read_raw(path) {
        Some(s) => s
            .trim_end_matches(['\n', '\r', ' ', '\t'])
            .to_string(),
        None => String::new(),
    }
}



fn read_string_any(paths: &[&str]) -> String {
    for p in paths {
        let v = read_str_node(p);
        if !v.is_empty() {
            return v;
        }
    }
    String::new()
}

fn read_int_any(paths: &[&str]) -> i32 {
    for p in paths {
        let v = read_long(p);
        if let Some(v) = v {
            return v as i32;
        }
    }
    0
}

fn write_string(path: &str, value: &str) {
    let _ = fs::write(path, value);
}

fn write_string_any(paths: &[&str], value: &str) {
    for p in paths {
        if read_raw(p).is_some() {
            write_string(p, value);
        }
    }
}




fn parse_charge_limit_state_payload(value: &str) -> (i32, Option<i32>) {
    let ints: Vec<i32> = value
        .split('+')
        .filter_map(|p| p.trim().parse::<i32>().ok())
        .collect();
    if ints.len() >= 3 && ints[0] == 2 {

        (ints[1], Some(ints[2]))
    } else if ints.len() >= 2 && ints[0] == 2 {

        (ints[1], None)
    } else if ints.len() >= 2 {
        (ints[0], Some(ints[1]))
    } else {
        (parse_first_int(value).unwrap_or(0), None)
    }
}



fn parse_charge_limit_control_payload(value: &str) -> (i32, Option<i32>, i32, Option<i32>) {
    let ints: Vec<i32> = value
        .split('+')
        .filter_map(|p| p.trim().parse::<i32>().ok())
        .collect();
    if ints.len() >= 5 && ints[0] == 4 {
        (ints[1], Some(ints[2]), ints[3], Some(ints[4]))
    } else if ints.len() >= 4 {
        (ints[0], Some(ints[1]), ints[2], Some(ints[3]))
    } else {
        (parse_first_int(value).unwrap_or(0), None, 0, None)
    }
}

fn parse_first_int(value: &str) -> Option<i32> {
    value
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .find(|part| !part.is_empty() && *part != "-")
        .and_then(|part| part.parse::<i32>().ok())
}


use std::sync::Arc;
struct DecimalState {
    last_cap: i32,
    last_dec: i32,
    last_raw_dec: i32,
    has_last: bool,
    was_charging: bool,
    hw_max_dec: i32,
    cache: String,
    last: Instant,
    charge_start: Instant,
    charge_start_valid: bool,
    wrap_hold_since: Option<Instant>,
}

pub struct Charger {
    decimal: Arc<Mutex<DecimalState>>,
    chg_up_limit_state: Arc<Mutex<String>>,
    chg_up_limit_value: Arc<Mutex<String>>,
    charge_limit_active: Arc<Mutex<bool>>,
    last_resume: Arc<Mutex<Option<Instant>>>,
}

impl Charger {
    pub fn new() -> Self {
        let now = Instant::now();
        Charger {
            decimal: Arc::new(Mutex::new(DecimalState {
                last_cap: -1,
                last_dec: 0,
                last_raw_dec: 0,
                has_last: false,
                was_charging: false,
                hw_max_dec: 0,
                cache: String::new(),
                last: now,
                charge_start: now,
                charge_start_valid: false,
                wrap_hold_since: None,
            })),
            chg_up_limit_state: Arc::new(Mutex::new("0".into())),
            chg_up_limit_value: Arc::new(Mutex::new("90".into())),
            charge_limit_active: Arc::new(Mutex::new(false)),
            last_resume: Arc::new(Mutex::new(None)),
        }
    }

    fn get_adapter_power_w(&self) -> i32 {


        read_int_any(&[
            "/sys/class/qcom-battery/apdo_max",
            "/sys/class/qcom-battery/power_max",
            "/sys/class/power_supply/usb/apdo_max",
            "/sys/class/power_supply/usb/power_max",
            "/sys/class/qcom-battery/referance_power",
        ])
    }

    fn get_quick_charge_type(&self) -> String {
        read_string_any(&[
            "/sys/class/qcom-battery/quick_charge_type",
            "/sys/class/power_supply/usb/quick_charge_type",
            "/sys/class/power_supply/battery/quick_charge_type",
        ])
    }

    fn get_usb_real_type(&self) -> String {
        read_string_any(&[
            "/sys/class/qcom-battery/usb_real_type",
            "/sys/class/power_supply/usb/type",
        ])
    }

    fn get_fastchg_mode(&self) -> i32 {
        read_int_any(&[
            "/sys/class/qcom-battery/fastchg_mode",
            "/sys/class/power_supply/bms/fastcharge_mode",
        ])
    }

    fn get_sport_mode(&self) -> i32 {
        read_int_node("/sys/class/qcom-battery/sport_mode")
    }

    fn get_usb_online(&self) -> i32 {
        read_int_or("/sys/class/power_supply/usb/online", 0)
    }

    fn is_data_port_usb_type(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_uppercase().as_str(),
            "USB" | "SDP" | "USB_SDP" | "CDP" | "USB_CDP" | "PC" | "PC_PORT"
        )
    }



    fn classify_fast_charge(&self) -> i32 {
        let online = self.get_usb_online() != 0;
        let usb_real = self.get_usb_real_type();
        if !online {
            return 0;
        }
        if Self::is_data_port_usb_type(&usb_real) {
            return 0;
        }
        let qct = self.get_quick_charge_type();
        let qct_num: i32 = qct.trim().parse().unwrap_or(-1);
        match qct_num {
            1 => return 1,
            2 => return 2,
            3 | 4 => return 3,
            _ => {}
        }
        let lower = qct.to_lowercase();
        if lower.contains("super") || lower.contains("turbo") {
            return 3;
        }
        if lower.contains("flash") {
            return 2;
        }
        if lower.contains("fast") {
            return 1;
        }
        if lower.contains("normal") {
            return 0;
        }
        if self.get_fastchg_mode() != 0 || self.get_sport_mode() != 0 {
            return 3;
        }
        let ul = usb_real.to_lowercase();
        if ul.contains("pd") || ul.contains("pps") {
            return 3;
        }
        if ul.contains("qc") || ul.contains("hvdcp") || ul.contains("quick") {
            return 2;
        }
        if ul.contains("dcp") {
            return 1;
        }
        if ul.contains("sdp") || ul.contains("cdp") {
            return 0;
        }
        let pw = self.get_adapter_power_w();
        if pw > 20 {
            3
        } else if pw > 10 {
            2
        } else if pw > 3 || online {
            1
        } else {
            0
        }
    }

    fn fast_type(&self) -> i32 {
        self.classify_fast_charge()
    }

    fn fast_type_tech(&self) -> i32 {

        let qct = self.get_quick_charge_type();
        let qct_num: i32 = qct.trim().parse().unwrap_or(-1);
        match qct_num {
            1 => return 1,
            2 => return 2,
            3 | 4 => return 3,
            _ => {}
        }
        let lower = qct.to_lowercase();
        if lower.contains("super") || lower.contains("turbo") {
            3
        } else if lower.contains("flash") {
            2
        } else if lower.contains("fast") {
            1
        } else {
            self.fast_type()
        }
    }

    fn is_fast_charging(&self) -> bool {
        let online = self.get_usb_online() != 0;
        online
            && !Self::is_data_port_usb_type(&self.get_usb_real_type())
            && (self.fast_type() > 0
                || self.get_fastchg_mode() != 0
                || self.get_sport_mode() != 0
                || self.get_adapter_power_w() >= 10)
    }

    fn is_svooc_active(&self) -> bool {
        let online = self.get_usb_online() != 0;
        online
            && !Self::is_data_port_usb_type(&self.get_usb_real_type())
            && (self.fast_type() >= 2
                || self.get_fastchg_mode() != 0
                || self.get_sport_mode() != 0
                || self.get_adapter_power_w() >= 20)
    }

    fn is_pps_active(&self) -> bool {
        let online = self.get_usb_online() != 0;
        let usb = self.get_usb_real_type().to_lowercase();
        let quick = self.get_quick_charge_type().to_lowercase();
        online
            && !Self::is_data_port_usb_type(&self.get_usb_real_type())
            && (self.fast_type_tech() >= 3
                || usb.contains("pd")
                || usb.contains("pps")
                || quick.contains("pd")
                || quick.contains("pps"))
    }

    fn should_show_power(&self) -> bool {
        self.power_available()
    }

    fn power_available(&self) -> bool {
        self.get_usb_online() != 0
            && !Self::is_data_port_usb_type(&self.get_usb_real_type())
            && self.get_adapter_power_w() > 0
    }


    pub fn is_charge_limit_active(&self) -> bool {
        self.charge_limit_switch_enabled()
            && *self.charge_limit_active.lock().unwrap()
    }

    fn charge_limit_switch_enabled(&self) -> bool {
        self.chg_up_limit_state
            .lock()
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap_or(0)
            != 0
    }

    fn charge_limit_target(&self) -> Option<i32> {
        self.chg_up_limit_value
            .lock()
            .unwrap()
            .trim()
            .parse::<i32>()
            .ok()
            .filter(|v| (10..=100).contains(v))
    }

    fn set_charge_limit_value(&self, value: &str) {
        hook_log(&format!("set_value payload={}", value));

        let (_, limit, _force, _recharge) = parse_charge_limit_control_payload(value);
        let parsed = limit.or_else(|| parse_first_int(value));
        if let Some(v) = parsed.filter(|v| (10..=100).contains(v)) {
            *self.chg_up_limit_value.lock().unwrap() = v.to_string();
        }
        if !self.charge_limit_switch_enabled() {
            return;
        }
        *self.last_resume.lock().unwrap() = None;
        let capacity = read_int_node("/sys/class/power_supply/battery/capacity");
        if capacity < 0 {
            return;
        }
        let suspend = self.charge_limit_target().is_some_and(|lim| capacity >= lim);
        let was_active = *self.charge_limit_active.lock().unwrap();
        hook_log(&format!("set_value cap={} suspend={} was_active={}", capacity, suspend, was_active));
        *self.charge_limit_active.lock().unwrap() = suspend;
        self.apply_charge_control_limit(suspend);
        if !suspend && was_active {
            *self.last_resume.lock().unwrap() = Some(Instant::now());
        }
    }

    fn set_charge_limit_state(&self, value: &str) {
        hook_log(&format!("set_state payload={}", value));
        let (enabled, limit) = parse_charge_limit_state_payload(value);
        *self.chg_up_limit_state.lock().unwrap() = enabled.to_string();
        if let Some(limit) = limit.filter(|v| (10..=100).contains(v)) {
            *self.chg_up_limit_value.lock().unwrap() = limit.to_string();
        }
        if enabled == 0 {
            hook_log(&format!("set_state enabled=0 -> DISABLE"));
            *self.charge_limit_active.lock().unwrap() = false;
            *self.last_resume.lock().unwrap() = None;
            self.apply_charge_control_limit(false);
        } else {
            *self.last_resume.lock().unwrap() = None;
            let capacity = read_int_node("/sys/class/power_supply/battery/capacity");
            if capacity < 0 {
                return;
            }
            let suspend = self.charge_limit_target().is_some_and(|lim| capacity >= lim);
            let was_active = *self.charge_limit_active.lock().unwrap();
            hook_log(&format!("set_state enabled={} cap={} suspend={} was_active={}", enabled, capacity, suspend, was_active));
            *self.charge_limit_active.lock().unwrap() = suspend;
            self.apply_charge_control_limit(suspend);
            if !suspend && was_active {
                *self.last_resume.lock().unwrap() = Some(Instant::now());
            }
        }
    }

    fn should_suspend(&self, capacity: i32) -> bool {
        if capacity < 0 {
            return *self.charge_limit_active.lock().unwrap();
        }
        let Some(limit) = self.charge_limit_target() else {
            return false;
        };
        let active = *self.charge_limit_active.lock().unwrap();
        if active {
            capacity >= limit - CHARGE_LIMIT_RESUME_DELTA
        } else {
            if let Some(resumed_at) = *self.last_resume.lock().unwrap() {
                if resumed_at.elapsed().as_secs() < RESUME_COOLDOWN_SECS {
                    return false;
                }
            }
            capacity >= limit
        }
    }

    fn apply_charge_control_limit(&self, restrict: bool) {
        let val = if restrict { "1" } else { "0" };
        let p = CHARGE_INPUT_SUSPEND_PATHS[0];
        let cur = read_str_node(p);
        if cur == val {
            return;
        }
        write_string(p, val);
    }

    pub fn monitor_tick(&self) {
        if !self.charge_limit_switch_enabled() {
            return;
        }
        let capacity = read_int_node("/sys/class/power_supply/battery/capacity");
        if capacity < 0 {
            return;
        }
        let should_suspend = self.should_suspend(capacity);
        let cur = *self.charge_limit_active.lock().unwrap();
        if should_suspend != cur {
            *self.charge_limit_active.lock().unwrap() = should_suspend;
            if !should_suspend {
                *self.last_resume.lock().unwrap() = Some(Instant::now());
            }
        }
        // Always reconcile sysfs state so kernel/ADSP resets on USB replug cannot bypass the limit.
        self.apply_charge_control_limit(should_suspend);
    }









    fn read_hw_soc_decimal() -> Option<i32> {
        for p in SOC_DECIMAL_PATHS {
            if let Some(v) = read_long(p) {
                if (0..=100).contains(&v) {
                    return Some(v.clamp(0, 99) as i32);
                }
            }
        }
        None
    }

    fn read_capacity_clamped() -> i32 {
        read_int_node("/sys/class/power_supply/battery/capacity").clamp(0, 100)
    }

    fn get_ui_soc_decimal(&self) -> String {
        let mut d = self.decimal.lock().unwrap();
        let now = Instant::now();
        let status = read_str_node("/sys/class/power_supply/battery/status");
        let charging = status == "Charging";

        if charging != d.was_charging {
            d.cache.clear();
            d.wrap_hold_since = None;
            if charging {
                d.charge_start = now;
                d.charge_start_valid = true;
            }
        }
        d.was_charging = charging;

        if !d.cache.is_empty() {
            let interval = if charging {
                let since_start = if d.charge_start_valid {
                    now.duration_since(d.charge_start).as_secs_f64()
                } else {
                    1e9
                };
                if since_start < DECIMAL_FAST_WINDOW {
                    DECIMAL_FAST_INTERVAL
                } else {
                    DECIMAL_CHARGING_INTERVAL
                }
            } else {
                DECIMAL_IDLE_INTERVAL
            };
            if now.duration_since(d.last).as_secs_f64() < interval {
                return d.cache.clone();
            }
        }
        d.last = now;

        // Read capacity & hardware ADSP soc_decimal consistently across boundary transitions.
        let mut capacity = Self::read_capacity_clamped();
        let mut hw_dec = Self::read_hw_soc_decimal();
        if hw_dec.is_some() {
            let cap_after = Self::read_capacity_clamped();
            if cap_after != capacity {
                capacity = cap_after;
                hw_dec = Self::read_hw_soc_decimal();
            }
        }

        if capacity >= 100 {
            d.last_cap = 100;
            d.last_dec = 0;
            d.last_raw_dec = 0;
            d.has_last = true;
            d.wrap_hold_since = None;
            d.cache = "10000,10000".to_string();
            return d.cache.clone();
        }

        // Xiaomi ADSP computes soc_decimal in charge_full_design scale (e.g. 5000mAh),
        // so on aged batteries (charge_full < charge_full_design) soc_decimal peaks at
        // ~(charge_full / charge_full_design) * 100 (e.g. ~61-65) when capacity ticks +1.
        // Scale raw soc_decimal by charge_full_design / charge_full and gently calibrate
        // against observed wrap peaks so the UI smoothly reaches ~99 right before capacity +1.
        let mut shown = if let Some(raw_dec) = hw_dec {
            let cf = read_long("/sys/class/power_supply/battery/charge_full").unwrap_or(0);
            let cfd = read_long("/sys/class/power_supply/battery/charge_full_design").unwrap_or(0);
            let base_max = if cf > 0 && cfd > 0 && cf < cfd {
                ((cf * 100 + cfd / 2) / cfd).clamp(35, 100) as i32
            } else {
                100
            };

            if d.hw_max_dec <= 0 {
                d.hw_max_dec = base_max;
            }

            // If capacity just ticked +1 while charging and raw_dec wrapped, refine hw_max_dec.
            if charging
                && d.has_last
                && capacity == d.last_cap + 1
                && raw_dec <= 15
                && d.last_raw_dec >= 30
            {
                let observed = (d.last_raw_dec + 2).clamp(35, 100);
                let lo = (base_max - 12).max(35);
                let hi = (base_max + 8).min(100);
                if observed >= lo && observed <= hi {
                    d.hw_max_dec = ((d.hw_max_dec * 2 + observed) / 3).clamp(lo, hi);
                }
            }

            d.last_raw_dec = raw_dec;
            let denom = d.hw_max_dec.max(1);
            if denom < 100 {
                ((raw_dec * 99 + denom / 2) / denom).clamp(0, 99)
            } else {
                raw_dec.clamp(0, 99)
            }
        } else {
            // Fallback for devices without hardware soc_decimal:
            // derive sub-percent directly from absolute charge_counter / charge_full
            // so unplug/replug never resets the decimal back to 00.
            let cc = read_long("/sys/class/power_supply/battery/charge_counter").unwrap_or(0);
            let cf = read_long("/sys/class/power_supply/battery/charge_full").unwrap_or(0);
            if cc > 0 && cf > 0 {
                (((cc as i128 * 10_000) / cf as i128) % 100).clamp(0, 99) as i32
            } else {
                0
            }
        };

        // Guard against 1-frame capacity/decimal wrap race while charging:
        // e.g. soc_decimal wrapped 99 -> 00 slightly before capacity bumped, or vice versa.
        if charging && d.has_last {
            if capacity == d.last_cap && d.last_dec >= 90 && shown <= 10 {
                let hold_start = *d.wrap_hold_since.get_or_insert(now);
                if now.duration_since(hold_start).as_secs_f64() < DECIMAL_WRAP_HOLD_SECS {
                    shown = 99;
                } else {
                    d.wrap_hold_since = None;
                }
            } else if capacity == d.last_cap + 1 && d.last_dec >= 90 && shown >= 90 {
                shown = 0;
                d.wrap_hold_since = None;
            } else {
                d.wrap_hold_since = None;
            }
        } else {
            d.wrap_hold_since = None;
        }

        shown = shown.clamp(0, 99);
        d.last_cap = capacity;
        d.last_dec = shown;
        d.has_last = true;

        let start_val = (capacity * 100 + shown).max(1);
        let end_val = if charging && shown < 99 {
            start_val + 1
        } else {
            start_val
        };
        d.cache = format!("{},{}", start_val, end_val);
        d.cache.clone()
    }
}

impl Clone for Charger {
    fn clone(&self) -> Self {
        Charger {
            decimal: Arc::clone(&self.decimal),
            chg_up_limit_state: Arc::clone(&self.chg_up_limit_state),
            chg_up_limit_value: Arc::clone(&self.chg_up_limit_value),
            charge_limit_active: Arc::clone(&self.charge_limit_active),
            last_resume: Arc::clone(&self.last_resume),
        }
    }
}

impl Default for Charger {
    fn default() -> Self {
        Self::new()
    }
}



impl Interface for Charger {}

impl ICharger for Charger {
    fn getChgConfig(
        &self,
        flag: ChgFuncFlag,
        _extra: &str,
        _callname: i32,
    ) -> BinderResult<String> {
        let out = if flag == ChgFuncFlag::GET_UI_CHG_ICON_TYPE {
            if self.power_available() { 3 } else { 0 }.to_string()
        } else if flag == ChgFuncFlag::GET_UI_POWER_VALUE
            || flag == ChgFuncFlag::GET_DEVICE_POWER_VALUE
            || flag == ChgFuncFlag::GET_ADAPTER_POWER_VALUE
        {
            if self.should_show_power() {
                self.get_adapter_power_w().to_string()
            } else {
                "0".to_string()
            }
        } else if flag == ChgFuncFlag::GET_CPA_POWER_VALUE {
            if self.should_show_power() && (self.is_pps_active() || self.get_sport_mode() != 0) {
                self.get_adapter_power_w().to_string()
            } else {
                "0".to_string()
            }
        } else if flag == ChgFuncFlag::IS_SUPPORT_SINGLE_HYPER_SPEED {
            if self.is_fast_charging() || self.is_svooc_active() {
                "1".to_string()
            } else {
                "0".to_string()
            }
        } else if flag == ChgFuncFlag::SET_CHG_UP_LIMIT_VALUE {
            self.chg_up_limit_value.lock().unwrap().clone()
        } else if flag == ChgFuncFlag::SET_CHG_UP_LIMIT_STATE {
            self.chg_up_limit_state.lock().unwrap().clone()
        } else if flag == ChgFuncFlag::GET_BATT_UI_CYCLE_COUNT || flag == ChgFuncFlag::GET_BATT_MANU_DATE {
            let mut v = read_int_node("/sys/class/power_supply/battery/cycle_count");
            if v < 0 {
                v = read_int_or("/sys/class/qcom-battery/fg1_cycle", 0);
            }
            v.to_string()
        } else if flag == ChgFuncFlag::GET_BATTERY_GAUGE_CAR_C {
            let mut v = read_milli_node("/sys/class/power_supply/battery/charge_counter", -1);
            if v < 0 {
                v = read_int_or("/sys/class/qcom-battery/fg1_rm", 0);
            }
            v.to_string()
        } else if flag == ChgFuncFlag::CHECK_FLAG_SUPPORT {
            "-1".to_string()
        } else {
            String::new()
        };
        Ok(out)
    }

    fn getBattAuthenticate(&self) -> BinderResult<i32> {
        Ok(1)
    }
    fn getPsyBatteryHmac(&self) -> BinderResult<i32> {
        Ok(1)
    }
    fn getPsyFastChgType(&self) -> BinderResult<i32> {
        Ok(self.fast_type())
    }
    fn getBattVoocChgIng(&self) -> BinderResult<i32> {
        Ok(if self.is_svooc_active() { 1 } else { 0 })
    }
    fn getBattPPSChgIng(&self) -> BinderResult<i32> {
        Ok(if self.is_pps_active() { 1 } else { 0 })
    }
    fn getBattPPSChgPower(&self) -> BinderResult<i32> {
        Ok(if self.should_show_power() && self.is_pps_active() {
            self.get_adapter_power_w()
        } else {
            0
        })
    }
    fn healthd_update_ui_soc_decimal(&self) -> BinderResult<String> {
        Ok(self.get_ui_soc_decimal())
    }
    fn testKitFeatureTest(&self, _index: i32) -> BinderResult<testKitFeatureTestResult> {
        Ok(testKitFeatureTestResult {
            r#ret: 0,
            r#str: String::new(),
        })
    }


    fn getBattParamNoplug(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getBccCsvData(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getBmsHeatingRunningStatus(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getChargerControl(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getDevinfoFastchg(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getPsyBatteryStatus(&self) -> BinderResult<String> {
        let s = read_str_node("/sys/class/power_supply/battery/status");
        Ok(if s.is_empty() { "Unknown".to_string() } else { s })
    }
    fn getPsyWirelessRX(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getPsyWirelessRxVersion(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getPsyWirelessTX(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getPsyWirelessTxVersion(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getQuickModeGain(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getReserveSocDebug(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getUisohDebugParameterInfo(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getWirelessDeviated(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getWirelessTXEnable(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn queryChargeInfo(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn queryWlsPencilInfo(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getChgOlcConfig(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getPsyBatterySN(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getBattGaugeInfo(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn testKitGetFeatureList(&self) -> BinderResult<String> {
        Ok(String::new())
    }
    fn testKitGetFeatureName(&self, _index: i32) -> BinderResult<String> {
        Ok(String::new())
    }
    fn getUsbCurrentEyeDiagram(&self, _model: i32) -> BinderResult<String> {
        Ok(String::new())
    }


    fn chgExchangeMesgInit(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn chgExchangeSohMesgInit(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getAcType(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getBattShortIcOtpStatus(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getBattSubCurrent(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getBatteryVoltageNow(&self) -> BinderResult<i32> {
        Ok(read_milli_node(
            "/sys/class/power_supply/battery/voltage_now",
            4000,
        ))
    }
    fn getBccExpStatus(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getBmsHeatingStatus(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getChargerCoolDown(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getChargerCriticalLog(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getChargerIdVolt(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getChargerLog(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getCustomSelectChgMode(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getFastCharge(&self) -> BinderResult<i32> {
        Ok(if self.is_fast_charging() { 1 } else { 0 })
    }
    fn getParallelChgMosTestResult(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyAcOnline(&self) -> BinderResult<i32> {
        Ok(read_int_or("/sys/class/power_supply/usb/online", 0))
    }
    fn getPsyBatteryCC(&self) -> BinderResult<i32> {
        let mut v = read_int_node("/sys/class/power_supply/battery/cycle_count");
        if v < 0 {
            v = read_int_or("/sys/class/qcom-battery/fg1_cycle", 0);
        }
        Ok(v)
    }
    fn getPsyBatteryCurrentNow(&self) -> BinderResult<i32> {
        Ok(read_milli_node(
            "/sys/class/power_supply/battery/current_now",
            0,
        ))
    }
    fn getPsyBatteryFcc(&self) -> BinderResult<i32> {
        let mut v = read_milli_node("/sys/class/power_supply/battery/charge_full", -1);
        if v < 0 {
            v = read_int_or("/sys/class/qcom-battery/fg1_fcc", 0);
        }
        Ok(v)
    }
    fn getPsyBatteryLevel(&self) -> BinderResult<i32> {
        Ok(read_int_or("/sys/class/power_supply/battery/capacity", 50))
    }
    fn getPsyBatteryNotify(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyBatteryPchg(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyBatteryPchgResetCount(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyBatteryRm(&self) -> BinderResult<i32> {
        let mut v = read_milli_node("/sys/class/power_supply/battery/charge_counter", -1);
        if v < 0 {
            v = read_int_or("/sys/class/qcom-battery/fg1_rm", 0);
        }
        Ok(v)
    }
    fn getPsyBatteryShortFeature(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyBatteryShortStatus(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyBatteryTemp(&self) -> BinderResult<i32> {
        Ok(read_int_or("/sys/class/power_supply/battery/temp", 300))
    }
    fn getPsyChargeTech(&self) -> BinderResult<i32> {
        Ok(self.fast_type_tech())
    }
    fn getPsyInputCurrent(&self) -> BinderResult<i32> {
        let mut v = read_milli_node("/sys/class/power_supply/usb/input_current_limit", -1);
        if v < 0 {
            v = read_milli_node("/sys/class/power_supply/usb/current_now", 0);
        }
        Ok(v)
    }
    fn getPsyOtgOnline(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyOtgSwitch(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyPcPortOnline(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyQGVbatDeviation(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getPsyTypeOrientation(&self) -> BinderResult<i32> {
        Ok(read_int_or("/sys/class/qcom-battery/cc_orientation", 0))
    }
    fn getPsyUsbOnline(&self) -> BinderResult<i32> {
        Ok(read_int_or("/sys/class/power_supply/usb/online", 0))
    }
    fn getPsyUsbStatus(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getQgVbatDeviation(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getSmartChgMode(&self) -> BinderResult<i32> {
        Ok(read_int_any(&[
            "/sys/class/qcom-battery/smart_chg",
            "/sys/class/power_supply/battery/smart_chg",
        ]))
    }
    fn getUIsohValue(&self) -> BinderResult<i32> {
        let mut soh: i32 = -1;
        let full = read_int_node("/sys/class/power_supply/battery/charge_full");
        let design = read_int_node("/sys/class/power_supply/battery/charge_full_design");
        if full > 0 && design > 0 {
            soh = ((full as i64 * 100 + design as i64 / 2) / design as i64) as i32;
        }
        if soh <= 0 {
            soh = read_int_node("/sys/class/qcom-battery/ui_soh");
        }
        if soh <= 0 {
            soh = read_int_node("/sys/class/qcom-battery/soh");
        }
        if soh <= 0 || soh > 100 {
            soh = 100;
        }
        Ok(soh)
    }
    fn getUsbInputCurrentNow(&self) -> BinderResult<i32> {
        Ok(read_milli_node("/sys/class/power_supply/usb/current_now", 0))
    }
    fn getUsbPrimalType(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWiredOtgOnline(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessAdapterPower(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessCapacity(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessChargePumpEn(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessCurrentNow(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessOnline(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessPenPresent(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessPtmcId(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessRXEnable(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessRealType(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessUserSleepMode(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getWirelessVoltageNow(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn updateUiSohToPartion(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getCpVbatDeviation(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn getChargingModeInGsmCall(&self) -> BinderResult<i32> {
        Ok(0)
    }
    fn testKitGetFeatureNum(&self) -> BinderResult<i32> {
        Ok(0)
    }


    fn VolDividerIcWorkModeSet(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChargeEMMode(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChargerControl(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChargerCoolDown(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChargerCriticalLog(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChargerCycle(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChargerFactoryModeTest(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChargerLog(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setFastchgFwUpdate(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setPsyMmiChgEn(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setPsyOtgSwitch(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setReserveSocDebug(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setShipMode(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setSmartChgMode(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setTbattPwrOff(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setUisohDebugInfo(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setUsbPrimalType(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWirelessChargePumpEn(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWirelessFtmMode(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWirelessIconDelay(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWirelessIdtAdcTest(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWirelessPenSoc(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWirelessRXEnable(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWirelessTXEnable(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWirelessUserSleepMode(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setWlsThirdPartitionInfo(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChgOlcConfig(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setSuperEnduranceStatus(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setSuperEnduranceCount(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setBobStatus(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setPsySlowChgEn(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setBatteryLogPush(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChargingModeInGsmCall(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChgRusConfig(&self, _data: &str) -> BinderResult<i32> {
        Ok(0)
    }


    fn nightstandby(&self, _status: i32) -> BinderResult<i32> {
        Ok(0)
    }
    fn notifyScreenStatus(&self, _status: i32) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChgStatusToBcc(&self, _status: i32) -> BinderResult<i32> {
        Ok(0)
    }
    fn setCustomSelectChgMode(&self, _mode: i32, _enable: bool) -> BinderResult<i32> {
        Ok(0)
    }
    fn setSmartCoolDown(
        &self,
        _cool_down: i32,
        _normal_cool_down: i32,
        _pkg_name: &str,
    ) -> BinderResult<i32> {
        Ok(0)
    }
    fn setChgConfig(
        &self,
        flag: ChgFuncFlag,
        extra: &str,
        _callname: i32,
    ) -> BinderResult<i32> {
        if flag == ChgFuncFlag::SET_CHG_UP_LIMIT_VALUE {
            self.set_charge_limit_value(extra);
        } else if flag == ChgFuncFlag::SET_CHG_UP_LIMIT_STATE {
            self.set_charge_limit_state(extra);
        }
        Ok(0)
    }
    fn setUsbEyeDiagram(
        &self,
        _model: i32,
        _eye_diagram: &str,
        _is_default_eye_diagram: bool,
    ) -> BinderResult<i32> {
        Ok(0)
    }
}
