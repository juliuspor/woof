use std::{env, path::PathBuf};

fn main() {
    let plist = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("Info.plist");
    println!("cargo:rerun-if-changed={}", plist.display());

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    for argument in [
        "-Xlinker".to_owned(),
        "-sectcreate".to_owned(),
        "-Xlinker".to_owned(),
        "__TEXT".to_owned(),
        "-Xlinker".to_owned(),
        "__info_plist".to_owned(),
        "-Xlinker".to_owned(),
        plist.display().to_string(),
    ] {
        println!("cargo:rustc-link-arg-bin=woof-mcp={argument}");
    }
}
