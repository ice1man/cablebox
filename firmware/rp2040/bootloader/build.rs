use std::{env, fs, path::PathBuf};

fn main() {
    // The linker looks for memory.x here: the shared layout, with this program's place in it.
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let layout = fs::read_to_string("../layout.x").unwrap();
    fs::write(out.join("memory.x"), layout + "REGION_ALIAS(\"FLASH\", BOOTLOADER);\n").unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=../layout.x");
    println!("cargo:rerun-if-env-changed=PROBE_STAGE");
}
