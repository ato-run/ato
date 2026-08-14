#!/bin/sh
printf '\033[2J\033[H'
printf 'Ato Pixel Stream Fixture\n\n'
printf 'Type text and press Enter. Pointer and US keyboard input are enabled.\n'
printf 'Clipboard, file transfer, audio, GPU, resize, and IME are disabled.\n\n'

while IFS= read -r line; do
  printf 'received: %s\n' "$line"
done
