#!/bin/sh

export HOME=/root

echo -e "Welcome to \e[96m\e[1mStarry OS\e[0m!"
env
echo

echo -e "Use \e[1m\e[3mapk\e[0m to install packages."
echo

# Do your initialization here!

cd /tests/target/riscv64gc-unknown-linux-musl/release/
./helloworld
cd /
rm -rf /tests
exit
# cd ~
# sh --login