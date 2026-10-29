#!/bin/sh
# Start an interactive shell with Pandoc once the Runner has sized the PTY.
# The Runner resizes the terminal right after the container starts; a prompt
# drawn before that would be redrawn, so the startup bytes would depend on
# timing. Each command runs once; its output and exit status stay on screen.
tries=0
while [ "$(stty size 2>/dev/null)" != "40 120" ]; do
	tries=$((tries + 1))
	if [ "$tries" -gt 1000 ]; then
		echo "terminal was not sized to 120x40" >&2
		exit 1
	fi
	sleep 0.01
done
# Saved outputs when the Run has an output grant; otherwise the Run's own
# tmpfs, which disappears when the Run ends.
if [ -n "$ATO_OUTPUT_DIR" ] && [ -d "$ATO_OUTPUT_DIR" ]; then
	cd "$ATO_OUTPUT_DIR" || exit 1
else
	cd /tmp || exit 1
fi
export PS1='pandoc$ '
exec /bin/sh -i
