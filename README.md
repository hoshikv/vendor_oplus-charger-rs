# vendor_oplus-hardware-charger

Rust AIDL HAL for OPlus charging on Xiaomi devices (ICharger V10).

It implements `vendor.oplus.hardware.charger.ICharger` version 10
(`@VintfStability`) and registers the instance:

```
vendor.oplus.hardware.charger.ICharger/default
```

The service is a drop-in replacement for the C++ HAL: it talks to the same
kernel power-supply sysfs nodes, uses the same AIDL interface, and does not
depend on `libbinder_ndk`. The only required shared libraries are `libc.so` and
`libdl.so`.

> This HAL reports charging state and applies the charge limit. It does not
> replace the kernel charger driver or the charging IC firmware.

## Features

### 1. Charging wattage

Wattage is read directly from the adapter/APDO power nodes, in watts:

```
/sys/class/qcom-battery/apdo_max
/sys/class/qcom-battery/power_max
/sys/class/power_supply/usb/apdo_max
/sys/class/power_supply/usb/power_max
/sys/class/qcom-battery/referance_power
```

The first readable node wins. The value is only reported when the USB port is
online, is not a data-only port (`USB`, `SDP`, `CDP`, `PC`, etc.), and the
adapter power is greater than zero.

| AIDL flag | Behavior |
|---|---|
| `GET_UI_POWER_VALUE` | UI charging wattage |
| `GET_DEVICE_POWER_VALUE` | Device charging wattage |
| `GET_ADAPTER_POWER_VALUE` | Adapter wattage |
| `GET_CPA_POWER_VALUE` | Adapter wattage when PPS or sport mode is active |
| `GET_UI_CHG_ICON_TYPE` | `3` when power is available, otherwise `0` |

The same value is returned by `getBattPPSChgPower()` when PPS is active.
`getBattPPSChgIng()` and `getBattVoocChgIng()` report whether PPS or
SuperVOOC-style charging is detected.

The Rust port reads the current wattage directly. It does not apply the C++
build's APDO display ramp.

### 2. Decimal battery SoC

`healthd_update_ui_soc_decimal()` returns two comma-separated integer
boundaries scaled by 100:

```
start,end
```

For example, `5037,5038` corresponds to the decimal SoC range around
50.37-50.38. The values are generated from:

- `/sys/class/power_supply/battery/capacity`
- `/sys/class/power_supply/battery/charge_counter`
- `/sys/class/power_supply/battery/charge_full`

The sub-percent step is calibrated from `charge_counter` as the battery moves
between integer capacity levels. A rolling five-sample history smooths the
measured step. If `charge_full` is unavailable, the fallback is `3374000` uAh.
The result is clamped to the valid range.

When the device is not charging on an external adapter, the fallback is
`capacity * 100, capacity * 100 + 1`.

While charging, updates run every 200 ms for the first 60 seconds, then every
1 second. The result is cached and recalculated on demand.

### 3. Boost / fast-charge detection

The HAL classifies the connected charger into OPlus fast-charge levels:

| Level | Meaning | Typical detection |
|---|---|---|
| `0` | Normal / not charging | data port, no adapter, or `normal` quick-charge type |
| `1` | Fast charging | DCP, `fast`, quick-charge type `1`, adapter power > 3 W |
| `2` | Flash / quick charging | `flash`, QC/HVDCP, quick-charge type `2`, adapter power > 10 W |
| `3` | Super / turbo / SVOOC / PPS | `super`, `turbo`, PD/PPS, quick-charge type `3` or `4`, `fastchg_mode`, `sport_mode`, adapter power > 20 W |

Detection uses:

```
/sys/class/qcom-battery/quick_charge_type
/sys/class/power_supply/usb/quick_charge_type
/sys/class/power_supply/battery/quick_charge_type
/sys/class/qcom-battery/usb_real_type
/sys/class/power_supply/usb/type
/sys/class/qcom-battery/fastchg_mode
/sys/class/power_supply/bms/fastcharge_mode
/sys/class/qcom-battery/sport_mode
/sys/class/power_supply/usb/online
```

The result is exposed through:

- `getPsyFastChgType()` and `getPsyChargeTech()`: level `0`-`3`
- `getFastCharge()`: `1` when fast charging
- `getBattVoocChgIng()`: `1` when SVOOC-level charging is active
- `getBattPPSChgIng()`: `1` when PD/PPS is active
- `IS_SUPPORT_SINGLE_HYPER_SPEED`: `1` when fast or SVOOC charging is active
- `GET_UI_CHG_ICON_TYPE`: `3` when adapter power is available

"Boost" here means fast-charge state reporting. The HAL does not write a
separate boost or turbo control node.

### 4. Charge limit

The charge limit is controlled through two AIDL flags:

| Flag | Value | Payload example |
|---|---|---|
| `SET_CHG_UP_LIMIT_STATE` (`14`) | `1` = enabled, `0` = disabled | `1+90` |
| `SET_CHG_UP_LIMIT_VALUE` (`5`) | Target percentage | `80` |

The default state is disabled (`0`) with a target of `90`. The value is kept in
memory and resets when the service restarts.

The payload parser accepts OPlus multi-field values separated by `+`. For the
state flag, `1+90` enables the limit at 90%. For the value flag, a plain integer
or a multi-field payload containing the limit is accepted.

When the target is reached, the HAL writes:

```
/sys/class/qcom-battery/input_suspend = 1
```

Charging resumes by writing `0`. To avoid short charge/discharge cycles:

- once suspended, charging stays suspended until capacity falls to
  `target - 5`
- after a resume, the limit is not re-applied for 60 seconds

A state value of `0` clears the target, disables the limit, and writes
`input_suspend = 0`.

### 5. Battery telemetry

The implementation also exposes battery data used by OPlus/ColorOS
components, including:

- cycle count and battery manufacture date
- charge counter and remaining charge
- battery state of charge and state of health
- battery voltage, current, and temperature
- USB input current and online state
- charger type and charge technology

Unsupported or unavailable AIDL methods return `0` or an empty string, matching
the fallback behavior expected by the framework.

## Binder client

`src/main.rs` includes a small client mode for testing and for applying the
charge limit without the framework:

```bash
# Enable the limit at 90%
./vendor.oplus.hardware.charger-V10-service --client 14 1+90

# Change the limit to 80%
./vendor.oplus.hardware.charger-V10-service --client 5 80
```

The client prints the current limit state, limit value, charging wattage,
charger icon type, CPA power, hyper-speed support, and decimal SoC. The service
must already be running and the caller must have binder access.

## Project structure

```
.
├── aidl/vendor/oplus/hardware/charger/
│   ├── ICharger.aidl                  # ICharger interface, version 10
│   ├── ChgFuncFlag.aidl               # getChgConfig/setChgConfig flags
│   └── testKitFeatureTestResult.aidl  # testKit result parcelable
├── src/
│   ├── main.rs                        # service/client entrypoint, binder setup
│   └── charger.rs                     # ICharger implementation and sysfs logic
├── build.rs                           # rsbinder-aidl code generation
├── build.sh                           # cargo build, copy, strip, inspect
├── Cargo.toml                         # crate metadata and dependencies
├── Cargo.lock                         # locked dependency versions
├── .cargo/config.toml                 # aarch64-linux-android NDK linker
├── README.md
└── LICENSE
```

### Source and porting notes

This repository is a Rust port of the C++ OPlus charger HAL from the
`vendor_oplus_hardware_charger-V10-qcom` source tree. The C++ reference contains
`Charger.h` and `main.cpp`; `src/charger.rs` and `src/main.rs` are the Rust
equivalents.

The AIDL files are LineageOS Project snapshots:

```
SPDX-FileCopyrightText: 2025 The LineageOS Project
SPDX-License-Identifier: Apache-2.0
```

They are copied verbatim and should not be edited. The Rust implementation adds
charge-limit handling and expanded fast-charge classification. It retains the
C++ decimal SoC estimator with additional calibration.

### Dependencies

- [`rsbinder`](https://crates.io/crates/rsbinder) `0.10`: Binder IPC without
  `libbinder_ndk`
- `libc` `0.2`: Android property and reboot syscalls
- `rsbinder-aidl` `0.10`: AIDL code generation at build time

## Build

Prerequisites:

```bash
rustup target add aarch64-linux-android
```

The build expects the Android NDK at `/opt/android-ndk`, with an
`aarch64-linux-android34-clang` toolchain. If the NDK is installed elsewhere,
update the linker paths in `.cargo/config.toml`. `build.sh` also uses `$NDK`
for `llvm-strip` and `llvm-readelf`; the default is `/opt/android-ndk`.

Build:

```bash
./build.sh
```

Output:

```
./vendor.oplus.hardware.charger-V10-service
```

The result is a stripped arm64 PIE executable that links only `libc.so` and
`libdl.so`.

## Deploy

1. Install the binary at:

   ```
   /vendor/bin/hw/vendor.oplus.hardware.charger-V10-service
   ```

2. Add an init service entry, for example:

   ```
   service vendor.oplus.charger /vendor/bin/hw/vendor.oplus.hardware.charger-V10-service
       class hal
       user root
       group system
   ```

3. Add a VINTF manifest fragment for the
   `vendor.oplus.hardware.charger.ICharger/default` instance, version `10`.

4. Add sepolicy for the service domain and for the `/sys/class/...` nodes it
   reads and writes.

### Binder driver selection

By default the service uses the binder driver path provided by `rsbinder`:
binderfs first, then `/dev/binder`. The path can be overridden with the
property:

```
persist.kv.charger.binder=/dev/vndbinder
```

The property is used only when the path exists. If the vendor image routes
vendor AIDL HALs through **vndbinder**, set this property or change the driver
path in `src/main.rs`.

## Antikang (anti-theft) gate

Before registering the service, `main()` calls `antikang_or_die()`. The service
requires both properties to be set to exactly these values:

| Property | Expected value |
|---|---|
| `ro.hoshikv.support` | `1` |
| `ro.kvports.wattage` | `1` |

If either property is missing or different, the service:

1. unlinks its own binary (`std::env::current_exe()` -> `remove_file`), then
2. reboots to the bootloader through `sys.powerctl=reboot,bootloader`, with a
   raw `reboot(2)` fallback.

### Provision the watermark before shipping

The HAL does not set these properties itself. They must already exist when the
service starts. add them in the ROM:

```
/vendor/build.prop
ro.hoshikv.support=1
ro.kvports.wattage=1
```

you can change these props with your own props or just nuke this feature by yourself 


If the properties not found on your roms, this feature trigger the device to fastboot/bootloader. but fine if this exist(this feature doesn't brick your device)

### sepolicy for the self-destruct path

The service domain needs:

- `set_prop(<domain>, powerctl_prop)` to trigger `sys.powerctl`, and/or
- `sys_boot` capability plus a `syscall` allow rule for the raw `reboot(2)`
  fallback
- `unlink` permission on its own executable file type

## License

This project is licensed under the
[GNU General Public License v2.0](LICENSE).

The AIDL interface snapshots are licensed under Apache-2.0 as stated in their
file headers.
