#!/usr/bin/env python3

import argparse
import re
import shutil
import subprocess
import sys

parser = argparse.ArgumentParser(description="QEMU runner")

parser.add_argument("elf_executable", help="Location of compiled ELF executable to run")
# QEMU options
parser.add_argument("--init", default="/bin/sh", help="Location of the init process (in the rootfs)")
parser.add_argument("--rootfs", default="moss.img", help="Location of the root filesystem image to use")
parser.add_argument("--cpu", default="cortex-a72")
parser.add_argument("--smp", default=4, help="Number of CPU cores to use")
parser.add_argument("--memory", default="2G")
parser.add_argument("--debug", action="store_true", help="Enable QEMU debugging")
parser.add_argument("--display", action="store_true", help="Add a display device to the VM")
parser.add_argument("--demangle", action="store_true", help="Add a demangled backtrace")



args = parser.parse_args()


elf_executable = args.elf_executable
bin_executable_location = elf_executable.replace(".elf", "") + ".bin"
# Convert the ELF executable to a binary format
subprocess.run(["aarch64-none-elf-objcopy", "-O", "binary", elf_executable, bin_executable_location], check=True)

append_args = ""

if args.init.split("/")[-1] in ["bash", "sh"]:
    append_args = f"--init={args.init} --init-arg=-i"
else:
    append_args = f"--init={args.init}"

default_args = {
    "-M": "virt,gic-version=3",
    "-initrd": args.rootfs,
    "-cpu": args.cpu,
    "-m": args.memory,
    "-smp": str(args.smp),
    "-rtc": "base=utc,clock=host",
    "-nographic": None,
    "-s": None,
    "-kernel": bin_executable_location,
    "-append": f"{append_args} --rootfs=ext4fs --automount=/dev,devfs --automount=/tmp,tmpfs --automount=/proc,procfs --automount=/sys,sysfs",
}

# Arguments that can appear multiple times (e.g. -device)
extra_args = ["-device", "virtio-rng-device"]

if args.debug:
    default_args["-S"] = None

if args.display:
    del default_args["-nographic"]
    default_args["-global"] = "virtio-mmio.force-legacy=false"
    default_args["-nic"] = "none"
    # Add uart
    default_args["-serial"] = "stdio"
    extra_args += ["-device", "virtio-gpu-device"]

qemu_command = ["qemu-system-aarch64"]

for key, value in default_args.items():
    qemu_command.append(key)
    if value is not None:
        qemu_command.append(value)

qemu_command += extra_args

if args.demangle:
    addr2line = (
        shutil.which("aarch64-none-elf-addr2line")
        or shutil.which("llvm-addr2line")
        or shutil.which("addr2line")
    )
    pc_pattern = re.compile(r"\bPC ([0-9a-fA-F]+)\b")
    decode_cache = {}


    def decode_pc(pc: str) -> str | None:
        if addr2line is None:
            return None

        pc = pc.lower()
        if pc in decode_cache:
            return decode_cache[pc]

        result = subprocess.run(
            [addr2line, "-e", elf_executable, "-f", "-C", "-p", f"0x{pc}"],
            check=False,
            capture_output=True,
            text=True,
        )

        decoded = result.stdout.strip() or None
        decode_cache[pc] = decoded
        return decoded


    with subprocess.Popen(
        qemu_command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    ) as proc:
        assert proc.stdout is not None

        for line in proc.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()

            match = pc_pattern.search(line)
            if match is None:
                continue

            decoded = decode_pc(match.group(1))
            if decoded is not None:
                print(f"\t=> {decoded}", flush=True)

        ret = proc.wait()
        if ret != 0:
            raise subprocess.CalledProcessError(ret, qemu_command)
else:
    subprocess.run(qemu_command, check=True)
