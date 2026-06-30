#!/usr/bin/env bash
# Build a Firecracker-bootable ext4 rootfs that boots READ-ONLY: the init mounts
# tmpfs over the writable dirs so the app needs no writable root.
set -uo pipefail
NAME="$1" IMG="$2" SETUP="$3" START="$4" SIZE_MB="$5"
cd ~/bench
echo "[$NAME] pull $IMG"; sudo docker pull -q "$IMG" >/dev/null
sudo docker rm -f "b-$NAME" >/dev/null 2>&1 || true
sudo docker run --name "b-$NAME" "$IMG" sh -c "$SETUP" || { echo "[$NAME] SETUP FAILED"; exit 1; }
sudo docker commit "b-$NAME" "snap-$NAME" >/dev/null
cid=$(sudo docker create "snap-$NAME"); sudo docker export "$cid" -o "/tmp/$NAME.tar"; sudo docker rm "$cid" >/dev/null
dst="$HOME/bench/$NAME.ext4"; rm -f "$dst"
dd if=/dev/zero of="$dst" bs=1M count="$SIZE_MB" status=none; mkfs.ext4 -q -F "$dst"
mnt="/mnt/$NAME"; sudo mkdir -p "$mnt"; sudo mount -o loop "$dst" "$mnt"
sudo tar -xf "/tmp/$NAME.tar" -C "$mnt"
sudo rm -f "$mnt/sbin/init"
sudo tee "$mnt/sbin/init" >/dev/null <<INIT
#!/bin/sh
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PYTHONDONTWRITEBYTECODE=1 HOME=/tmp
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mount -t tmpfs tmpfs /tmp 2>/dev/null
mount -t tmpfs tmpfs /run 2>/dev/null
mount -t tmpfs tmpfs /var/tmp 2>/dev/null
( $START ) >/tmp/app.log 2>&1 &
while true; do sleep 1000; done
INIT
sudo chmod +x "$mnt/sbin/init"
sync; sudo umount "$mnt"
echo "[$NAME] built $(du -h "$dst"|cut -f1) ro-capable rootfs"
