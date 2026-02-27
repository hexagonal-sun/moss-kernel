#!/usr/bin/env python3

import argparse
import subprocess

parser = argparse.ArgumentParser(description="QEMU runner")

parser.add_argument("elf_executable", help="Location of compiled ELF executable to run")
# QEMU options
parser.add_argument("--init", default="/bin/sh", help="Location of the init process (in the rootfs)")
parser.add_argument("--rootfs", default="moss.img", help="Location of the root filesystem image to use")
parser.add_argument("--cpu", default="cortex-a72")
parser.add_argument("--smp", default=4, help="Number of CPU cores to use")
parser.add_argument("--memory", default="2G")
parser.add_argument("--debug", action="store_true", help="Enable QEMU debugging")



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
    "-nographic": None,
    "-s": None,
    "-kernel": bin_executable_location,
    "-append": f"{append_args} --rootfs=ext4fs --automount=/dev,devfs --automount=/tmp,tmpfs --automount=/proc,procfs --automount=/sys,sysfs"
}

if args.debug:
    default_args["-S"] = None

qemu_command = ["qemu-system-aarch64"]

for key, value in default_args.items():
    qemu_command.append(key)
    if value is not None:
        qemu_command.append(value)

subprocess.run(qemu_command, check=True)
