use std::{env, fs, path::PathBuf};

fn main() {
    // The linker looks for memory.x here: the shared layout, with this program's place in it.
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // DIAGNOSTIC: FIRMWARE_STANDALONE links the application to start from flash on its own.
    println!("cargo:rerun-if-env-changed=FIRMWARE_STANDALONE");
    let (file, region) = match env::var_os("FIRMWARE_STANDALONE") {
        Some(_) => ("layout-standalone.x", "APP"),
        None => ("layout.x", "ACTIVE"),
    };
    println!("cargo:rerun-if-changed=layout-standalone.x");
    let layout = fs::read_to_string(file).unwrap();
    fs::write(out.join("memory.x"), layout + &format!("REGION_ALIAS(\"FLASH\", {region});\n")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=layout.x");
}
