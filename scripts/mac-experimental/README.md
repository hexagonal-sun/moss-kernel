# Experimental macOS build scripts

This is the folder for experimental macOS build support on Apple Silicon (we do not plan to support x86_64 macOS).

These scripts are not guaranteed to work and are not actively supported by the maintainer or other contributors. 
If you do have any issues, please mention @IAmTheNerdNextDoor in GitHub Issues.

### Dependencies
Regardless of stdlib, you will need:
- [Rustup](https://rustup.rs) (DO NOT install Rustup from Homebrew as you will incur conflicts. If you have Rustup installed from Homebrew, uninstall it and reinstall from the Rustup site.)
- [aarch64-none-elf](https://developer.arm.com/downloads/-/arm-gnu-toolchain-downloads)
- [Homebrew](https://brew.sh)
- QEMU (`brew install qemu`)
- e2fsprogs (`brew install e2fsprogs`)

If building for the GNU library:
- aarch64-unknown-linux-gnu (`brew tap messense/macos-cross-toolchains && brew install aarch64-unknown-linux-gnu`)

If building for the Musl library:
- Zig (`brew install zig`)

We use Zig to cross-compile to Linux musl on macOS.

When it comes to Rust, you need the nightly toolchain. To install it, run:
`rustup default nightly`

### Building
You can start building the initrd programs using this script:
`./scripts/mac-experimental/build-deps.sh`

musl programs are built by default.

If you want GNU program builds:
`stdlib=gnu ./scripts/mac-experimental/build-deps.sh`

After building, you can make the initrd with:
`./scripts/mac-experimental/create-image.sh`

The initrd should now be at `./moss.img`.

You can then build and run moss with:
`cargo run --release`

If you get an error about `aarch64-unknown-none-softfloat` during building, you need to install the target:
`rustup target install aarch64-unknown-none-softfloat`

Have fun!
