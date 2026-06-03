#!/bin/bash
set -e

# Build User Space
echo "Building user space..."
cd user
PATH="$HOME/.rustup/toolchains/nightly-aarch64-apple-darwin/bin:$PATH" cargo build
cd ..

# Package into initrd
echo "Packaging initrd..."
mkdir -p test_root/bin
cp user/target/riscv64gc-unknown-none-elf/debug/init test_root/init
cp user/target/riscv64gc-unknown-none-elf/debug/sh test_root/sh
cp user/target/riscv64gc-unknown-none-elf/debug/ls test_root/ls
cp user/target/riscv64gc-unknown-none-elf/debug/cat test_root/cat
cp user/target/riscv64gc-unknown-none-elf/debug/producer test_root/producer
cp user/target/riscv64gc-unknown-none-elf/debug/consumer test_root/consumer
cp user/target/riscv64gc-unknown-none-elf/debug/forktest test_root/forktest
cp user/target/riscv64gc-unknown-none-elf/debug/hello test_root/hello
cp user/target/riscv64gc-unknown-none-elf/debug/doexec test_root/doexec
echo "Hello from initrd TAR!" > test_root/dummy.txt
cd test_root
tar -cf ../test_root.tar dummy.txt init sh ls cat producer consumer forktest hello doexec
cd ..

# Run kernel
echo "Running kernel..."
PATH="$HOME/.rustup/toolchains/nightly-aarch64-apple-darwin/bin:$PATH" cargo run
