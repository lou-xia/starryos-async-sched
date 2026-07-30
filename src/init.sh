#!/bin/sh

export HOME=/root

echo -e "Welcome to \e[96m\e[1mStarry OS\e[0m!"
env
echo

echo -e "Use \e[1m\e[3mapk\e[0m to install packages."
echo

TEST_DIR=/tests/target/riscv64gc-unknown-linux-musl/release
echo "Running vsched2_test from $TEST_DIR"
cd $TEST_DIR
./vsched2_test

cd ~
sh --login
