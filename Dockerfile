FROM ubuntu:latest AS base

# Install dependencies
RUN apt update
RUN apt install -y build-essential curl git wget
RUN apt install -y qemu-system-aarch64 dosfstools mtools

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# Copy just ./scripts/
RUN mkdir /moss
WORKDIR /moss
RUN mkdir /moss/scripts
COPY ./scripts /moss/scripts
RUN ./scripts/download-arm-toolchain.sh /tmp/arm-toolchain.tar.xz
RUN mkdir -p /opt/arm-toolchain
RUN tar -xf /tmp/arm-toolchain.tar.xz -C /opt/arm-toolchain --strip-components=1
RUN rm /tmp/arm-toolchain.tar.xz

# Copy the current directory contents into the container at /moss
COPY . /moss

# Install ARM toolchain
ENV PATH="/opt/arm-toolchain/bin:${PATH}"

# Build root fs image
FROM base as image-builder
RUN ./scripts/build-deps.sh
RUN ./scripts/create-image.sh

# Build final image
FROM base as final
COPY --from=image-builder /moss/moss.img /moss/moss.img
RUN cargo build --release
