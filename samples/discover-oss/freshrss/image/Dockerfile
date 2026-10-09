# Thin derivative of the official FreshRSS image for the ato.run Runner OCI
# sandbox: non-root user, read-only root filesystem, all capabilities dropped.
# Only /var/www/FreshRSS/data (persistent state) and /tmp are written at runtime.
FROM docker.io/freshrss/freshrss:1.30.0@sha256:258b8edfc8a76a61f60d2d6a14d8f8d12495d78abf38646a2137612dfa264a21

# Unprivileged port; Apache runtime files move from /var/run, /var/lock and
# /var/log into /tmp. PHP sessions move from /var/lib/php/sessions into /tmp;
# Debian leaves their cleanup to cron, so PHP's own GC is re-enabled.
# DocumentRoot stays /var/www/FreshRSS/p.
RUN sed -r -i 's#^Listen .*#Listen 8080#' /etc/apache2/sites-available/FreshRSS.Apache.conf \
	&& sed -r -i \
		-e 's#^export APACHE_PID_FILE=.*#export APACHE_PID_FILE=/tmp/apache2/apache2.pid#' \
		-e 's#^export APACHE_RUN_DIR=.*#export APACHE_RUN_DIR=/tmp/apache2#' \
		-e 's#^export APACHE_LOCK_DIR=.*#export APACHE_LOCK_DIR=/tmp/apache2#' \
		-e 's#^export APACHE_LOG_DIR=.*#export APACHE_LOG_DIR=/tmp/apache2#' \
		/etc/apache2/envvars \
	&& for dir in /etc/php/*/apache2/conf.d; do \
		printf 'session.save_path = "/tmp"\nsession.gc_probability = 1\n' >"$dir/99-ato-runtime.ini"; \
	done

COPY --chmod=0755 entrypoint.sh /usr/local/bin/ato-freshrss-entrypoint

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/ato-freshrss-entrypoint"]
CMD ["apache2", "-D", "FOREGROUND"]
