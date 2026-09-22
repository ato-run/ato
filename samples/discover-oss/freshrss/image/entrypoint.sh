#!/bin/sh
# Replaces the official Docker/entrypoint.sh, which edits /etc and starts the
# cron daemon as root. Here nothing outside /var/www/FreshRSS/data and /tmp is
# written, and feed refresh is FreshRSS's own actualize_script.php on a loop
# (every FRESHRSS_ACTUALIZE_INTERVAL_S seconds from container start).
set -eu

cd /var/www/FreshRSS
mkdir -p /tmp/apache2
php -f ./cli/prepare.php >/dev/null

interval="${FRESHRSS_ACTUALIZE_INTERVAL_S:-3600}"
(
	while sleep "$interval"; do
		if php -f ./app/actualize_script.php >/dev/null; then
			echo "ato-freshrss: actualize ok $(date -u +%FT%TZ)" >&2
		else
			echo "ato-freshrss: actualize failed $(date -u +%FT%TZ)" >&2
		fi
	done
) &

# envvars reads unset variables such as APACHE_CONFDIR.
set +u
. /etc/apache2/envvars
exec "$@"
