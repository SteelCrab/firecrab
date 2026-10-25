FROM postgres:17
RUN apt-get update && apt-get install -y --no-install-recommends mariadb-client ca-certificates e2fsprogs python3 openssh-client fakeroot busybox-static file binutils zstd xz-utils lz4 lzop \
    && rm -rf /var/lib/apt/lists/*
ENTRYPOINT []
