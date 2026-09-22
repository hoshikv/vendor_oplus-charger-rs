

#![allow(non_snake_case)]

use std::fs;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn hook_log(msg: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() % 100_000)
        .unwrap_or(0);
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .write(true)
        .open("/data/local/tmp/chg_hook.log")
        .and_then(|mut f| {
            use std::io::Write;
            writeln!(f, "[{}] {}", now, msg)
        });
}

use rsbinder::{BinderResult, Interface};

use crate::aidl_bindings::vendor::oplus::hardware::charger::ChgFuncFlag::ChgFuncFlag;
use crate::aidl_bindings::vendor::oplus::hardware::charger::ICharger::ICharger;
use crate::aidl_bindings::vendor::oplus::hardware::charger::testKitFeatureTestResult::testKitFeatureTestResult;

const DECIMAL_FAST_WINDOW: f64 = 60.0;
const DECIMAL_FAST_INTERVAL: f64 = 0.20;
const DECIMAL_SLOW_INTERVAL: f64 = 1.0;
const SPAN_HISTORY_SIZE: usize = 5;

const CHARGE_CONTROL_EN_PATHS: &[&str] = &[
    "/sys/class/qcom-battery/charge_control_en",
    "/sys/class/power_supply/battery/charge_control_en",
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
    "/sys/class/power_supply/usb/input_suspend",
    "/sys/class/power_supply/main/input_suspend",
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
    init: bool,
    capacity: i32,
    cc_percent_start: i64,
    cc_span: i64,
    span_history: [i64; SPAN_HISTORY_SIZE],
    span_count: usize,
    was_charging: bool,
    cache: String,
    last: Instant,
    charge_start: Instant,
    charge_start_valid: bool,
}

pub struct Charger {
    decimal: Arc<Mutex<DecimalState>>,
    chg_up_limit_state: Arc<Mutex<String>>,
    chg_up_limit_value: Arc<Mutex<String>>,
    charge_limit_active: Arc<Mutex<bool>>,
    last_resume: Arc<Mutex<Instant>>,
}

impl Charger {
    pub fn new() -> Self {
        let now = Instant::now();
        Charger {
            decimal: Arc::new(Mutex::new(DecimalState {
                init: false,
                capacity: -1,
                cc_percent_start: 0,
                cc_span: 0,
                span_history: [0; SPAN_HISTORY_SIZE],
                span_count: 0,
                was_charging: false,
                cache: String::new(),
                last: now,
                charge_start: now,
                charge_start_valid: false,
            })),
            chg_up_limit_state: Arc::new(Mutex::new("0".into())),
            chg_up_limit_value: Arc::new(Mutex::new("90".into())),
            charge_limit_active: Arc::new(Mutex::new(false)),
            last_resume: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(RESUME_COOLDOWN_SECS))),
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
        self.chg_up_limit_state.lock().unwrap().trim() == "1"
    }

    fn charge_limit_target(&self) -> Option<i32> {
        self.chg_up_limit_value.lock().unwrap().trim().parse::<i32>().ok()
    }

    fn set_charge_limit_value(&self, value: &str) {
        hook_log(&format!("set_value payload={}", value));

        let (_, limit, _force, _recharge) = parse_charge_limit_control_payload(value);
        let parsed = limit.or_else(|| parse_first_int(value));
        let normalized = parsed.map(|v| v.to_string()).unwrap_or_else(|| value.to_string());
        *self.chg_up_limit_value.lock().unwrap() = normalized;
        if !self.charge_limit_switch_enabled() {
            return;
        }
        *self.last_resume.lock().unwrap() = Instant::now() - Duration::from_secs(RESUME_COOLDOWN_SECS + 1);
        let capacity = read_int_node("/sys/class/power_supply/battery/capacity");
        let suspend = self.should_suspend(capacity);
        let was_active = *self.charge_limit_active.lock().unwrap();
        hook_log(&format!("set_value cap={} suspend={} was_active={}", capacity, suspend, was_active));
        *self.charge_limit_active.lock().unwrap() = suspend;
        self.apply_charge_control_limit(suspend);
        if !suspend && was_active {

            *self.last_resume.lock().unwrap() = Instant::now();
        }
    }

    fn set_charge_limit_state(&self, value: &str) {
        hook_log(&format!("set_state payload={}", value));
        let (enabled, limit) = parse_charge_limit_state_payload(value);
        *self.chg_up_limit_state.lock().unwrap() = enabled.to_string();
        if let Some(limit) = limit {
            *self.chg_up_limit_value.lock().unwrap() = limit.to_string();
        }
        if enabled == 0 {
            hook_log(&format!("set_state enabled=0 -> DISABLE"));
            *self.charge_limit_active.lock().unwrap() = false;
            self.apply_charge_control_limit(false);
        } else {

            *self.last_resume.lock().unwrap() = Instant::now() - Duration::from_secs(RESUME_COOLDOWN_SECS + 1);
            let capacity = read_int_node("/sys/class/power_supply/battery/capacity");
            let suspend = self.should_suspend(capacity);
            let was_active = *self.charge_limit_active.lock().unwrap();
            hook_log(&format!("set_state enabled={} cap={} suspend={} was_active={}", enabled, capacity, suspend, was_active));
            *self.charge_limit_active.lock().unwrap() = suspend;
            self.apply_charge_control_limit(suspend);
            if !suspend && was_active {
                *self.last_resume.lock().unwrap() = Instant::now();
            }
        }
    }

    fn should_suspend(&self, capacity: i32) -> bool {
        let Some(limit) = self.charge_limit_target() else {
            return false;
        };
        let active = *self.charge_limit_active.lock().unwrap();
        if active {

            capacity >= limit - CHARGE_LIMIT_RESUME_DELTA
        } else {

            let elapsed = self.last_resume.lock().unwrap().elapsed().as_secs();
            if elapsed < RESUME_COOLDOWN_SECS {
                return false;
            }
            capacity >= limit
        }
    }

    fn apply_charge_control_limit(&self, restrict: bool) {
        let val = if restrict { "1" } else { "0" };

        for p in CHARGE_INPUT_SUSPEND_PATHS {
            let cur = read_str_node(p);
            if cur == val {
                continue;
            }
            hook_log(&format!("apply_limit {} {} (was {})", p, val, cur));
            write_string(p, val);
        }
    }

    pub fn monitor_tick(&self) {
        if !self.charge_limit_switch_enabled() {
            if *self.charge_limit_active.lock().unwrap() {
                *self.charge_limit_active.lock().unwrap() = false;
                self.apply_charge_control_limit(false);
            }
            return;
        }
        let capacity = read_int_node("/sys/class/power_supply/battery/capacity");
        let next = self.should_suspend(capacity);
        let cur = *self.charge_limit_active.lock().unwrap();
        if next != cur {
            *self.charge_limit_active.lock().unwrap() = next;
            self.apply_charge_control_limit(next);
            if !next {
                *self.last_resume.lock().unwrap() = Instant::now();
            }
        } else if next {

            let cur_is = read_int_node(&CHARGE_INPUT_SUSPEND_PATHS[0]);
            if cur_is != 1 {
                self.apply_charge_control_limit(true);
            }
        }
    }

    fn get_ui_soc_decimal(&self) -> String {
        if !self.power_available() {
            let mut cap = read_int_node("/sys/class/power_supply/battery/capacity");
            if cap < 0 {
                cap = 0;
            }
            if cap > 100 {
                cap = 100;
            }
            return format!("{},{}", cap * 100, cap * 100 + 1);
        }
        let mut d = self.decimal.lock().unwrap();
        let now = Instant::now();
        let status = read_str_node("/sys/class/power_supply/battery/status");
        let charging = status == "Charging";

        if charging && !d.was_charging {
            d.cache.clear();
            d.charge_start = now;
            d.charge_start_valid = true;
        }
        d.was_charging = charging;

        if !d.cache.is_empty() {
            let since_start = if d.charge_start_valid {
                now.duration_since(d.charge_start).as_secs_f64()
            } else {
                1e9
            };
            let interval = if charging && since_start < DECIMAL_FAST_WINDOW {
                DECIMAL_FAST_INTERVAL
            } else {
                DECIMAL_SLOW_INTERVAL
            };
            if now.duration_since(d.last).as_secs_f64() < interval {
                return d.cache.clone();
            }
        }
        d.last = now;

        let mut capacity = read_int_node("/sys/class/power_supply/battery/capacity");
        if capacity < 0 {
            capacity = 0;
        }
        if capacity > 100 {
            capacity = 100;
        }

        let cc_opt = read_long("/sys/class/power_supply/battery/charge_counter");
        let have_cc = matches!(cc_opt, Some(v) if v > 0);
        let cc = cc_opt.unwrap_or(0);
        let cf = match read_long("/sys/class/power_supply/battery/charge_full") {
            Some(v) if v > 0 => v,
            _ => 3_374_000,
        };

        let expected_step = if cf > 100 { cf / 100 } else { 34_650 };

        if have_cc {
            if !d.init {
                d.cc_percent_start = cc;
                d.capacity = capacity;
                d.init = true;
            } else if capacity > d.capacity {

                let steps = (capacity - d.capacity).max(1) as i64;
                let total_span = cc - d.cc_percent_start;
                let measured_step = if steps > 0 { total_span / steps } else { total_span };

                let min_valid = expected_step / 4;
                let max_valid = expected_step * 4;
                if measured_step > min_valid && measured_step < max_valid && measured_step > 0 {

                    let idx = d.span_count % SPAN_HISTORY_SIZE;
                    d.span_history[idx] = measured_step;
                    d.span_count += 1;
                    let count = d.span_count.min(SPAN_HISTORY_SIZE);
                    let sum: i64 = d.span_history[..count].iter().sum();
                    d.cc_span = sum / count as i64;
                }

                d.cc_percent_start = cc;
                d.capacity = capacity;
            } else if capacity < d.capacity {

                let span = if d.cc_span > 0 { d.cc_span } else { expected_step };
                d.cc_percent_start = cc - span;
                d.capacity = capacity;
            } else if capacity == d.capacity && d.cc_span == 0 && d.init {

                d.cc_span = expected_step;
            }
        }

        let mut span = if d.cc_span > 0 { d.cc_span } else { expected_step };
        if span <= 0 {
            span = 1;
        }
        let mut off = cc - d.cc_percent_start;
        if off < 0 {
            off = 0;
        }
        if off > span {
            off = span;
        }

        let mut shown = if have_cc {
            ((off * 99 + span / 2) / span) as i32
        } else {
            0
        };
        if shown < 0 {
            shown = 0;
        }
        if shown > 99 {
            shown = 99;
        }
        let mut end_dec = shown + 1;
        if end_dec > 99 {
            end_dec = 99;
        }
        let mut start_val = capacity * 100 + shown;
        let mut end_val = capacity * 100 + end_dec;
        if start_val < 1 {
            start_val = 1;
        }
        if end_val < start_val {
            end_val = start_val;
        }
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
