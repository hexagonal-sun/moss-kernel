use std::path::PathBuf;

fn main() {
    let linker_script = match std::env::var("CARGO_CFG_TARGET_ARCH") {
        Ok(arch) if arch == "aarch64" => PathBuf::from("./src/arch/arm64/boot/linker.ld"),
        Ok(arch) => {
            println!("Unsupported arch: {arch}");
            std::process::exit(1);
        }
        Err(_) => unreachable!("Cargo should always set the arch"),
    };

    println!("cargo::rerun-if-changed={}", linker_script.display());
    println!("cargo::rustc-link-arg=-T{}", linker_script.display());
}
