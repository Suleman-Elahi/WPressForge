# Per-site PHP-FPM image: WordPress extensions, WP-CLI, and a socket the host
# Nginx can reach through the site's bind mount.
#
#   docker build --build-arg PHP_VERSION=8.4 -t wp-panel/php-fpm:8.4 .
ARG PHP_VERSION=8.4
FROM php:${PHP_VERSION}-fpm-alpine

RUN set -eux; \
    apk add --no-cache \
        icu-libs libpng libjpeg-turbo libwebp freetype libzip zlib \
        imagemagick mariadb-client less bash; \
    apk add --no-cache --virtual .build-deps \
        $PHPIZE_DEPS icu-dev libpng-dev libjpeg-turbo-dev libwebp-dev \
        freetype-dev libzip-dev oniguruma-dev; \
    docker-php-ext-configure gd --with-jpeg --with-webp --with-freetype; \
    docker-php-ext-install -j"$(nproc)" \
        bcmath exif gd intl mysqli opcache pdo_mysql zip; \
    pecl install redis apcu; \
    docker-php-ext-enable redis apcu; \
    apk del .build-deps

# WP-CLI, executed by the agent inside this container.
RUN curl -fsSL -o /usr/local/bin/wp \
        https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar \
    && chmod +x /usr/local/bin/wp

COPY php.ini /usr/local/etc/php/conf.d/wp-panel.ini
COPY www.conf /usr/local/etc/php-fpm.d/zz-wp-panel.conf

WORKDIR /var/www/html
EXPOSE 9000
