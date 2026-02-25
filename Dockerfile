# Stage 1: Build Rust binary
FROM rust:1-bookworm AS builder

WORKDIR /build

# Cache dependencies separately from source
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release
RUN rm -rf src

# Build real binary
COPY src/ ./src/
RUN touch src/main.rs && cargo build --release


# Stage 2: Runtime
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y \
    irssi \
    sqlite3 \
    wget \
    cmake \
    build-essential \
    libwebsockets-dev \
    libjson-c-dev \
    libssl-dev \
    golang \
    libsqlite3-dev \
    scdoc \
    git \
    locales \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Build ttyd from source
RUN git clone --depth 1 https://github.com/tsl0922/ttyd.git /tmp/ttyd \
    && cmake -S /tmp/ttyd -B /tmp/ttyd/build \
    && cmake --build /tmp/ttyd/build \
    && cp /tmp/ttyd/build/ttyd /usr/local/bin/ttyd \
    && rm -rf /tmp/ttyd

# Install soju + sojuctl from source
RUN git clone --depth 1 --branch v0.9.0 https://codeberg.org/emersion/soju.git /tmp/soju \
    && cd /tmp/soju \
    && GOFLAGS="-tags=libsqlite3" make \
    && cp soju sojuctl /usr/local/bin/ \
    && rm -rf /tmp/soju

# UTF-8 locale
RUN sed -i '/en_US.UTF-8/s/^# //g' /etc/locale.gen && locale-gen en_US.UTF-8

ENV LANG=en_US.UTF-8 \
    LC_ALL=en_US.UTF-8 \
    LANGUAGE=en_US:en

RUN useradd -r -m -s /bin/bash -u 1010 irssiuser

WORKDIR /app

COPY --from=builder --chown=irssiuser:irssiuser \
    /build/target/release/irssi-v5 ./irssi-v5
COPY --chown=irssiuser:irssiuser public/ ./public/

RUN mkdir -p /data/sessions /soju \
    && chown -R irssiuser:irssiuser /app /data /soju

USER irssiuser

EXPOSE 3001

HEALTHCHECK --interval=30s --timeout=3s --start-period=15s --retries=3 \
    CMD wget -q --spider http://localhost:3001 || exit 1

CMD ["./irssi-v5"]