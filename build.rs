use std::path::PathBuf;
use time::OffsetDateTime;
use time::macros::format_description;

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

    // Set an environment variable with the date and time of the build
    let now = OffsetDateTime::now_utc();
    let format = format_description!(
        "[weekday repr:short] [month repr:short] [day] [hour]:[minute]:[second] UTC [year]"
    );
    let timestamp = now.format(&format).unwrap();
    #[cfg(feature = "smp")]
    println!("cargo:rustc-env=MOSS_VERSION=#1 Moss SMP {timestamp}");
    #[cfg(not(feature = "smp"))]
    println!("cargo:rustc-env=MOSS_VERSION=#1 Moss {timestamp}");
}
