use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=aidl");

    rsbinder_aidl::Builder::new()
        .source(PathBuf::from("aidl/vendor/oplus/hardware/charger/ICharger.aidl"))
        .source(PathBuf::from("aidl/vendor/oplus/hardware/charger/ChgFuncFlag.aidl"))
        .source(PathBuf::from("aidl/vendor/oplus/hardware/charger/testKitFeatureTestResult.aidl"))
        .include_dir(PathBuf::from("aidl"))
        .version(10)
        .set_async_support(false)
        .output(PathBuf::from("charger.rs"))
        .generate()
        .expect("AIDL codegen failed");
}
