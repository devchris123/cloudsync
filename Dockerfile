FROM rust:1.91.1 AS builder

WORKDIR /app

# Copy manifests                                                                                                           
COPY Cargo.toml Cargo.lock ./
COPY crates/cloudsync-common/Cargo.toml crates/cloudsync-common/Cargo.toml                                                 
COPY crates/cloudsync-server/Cargo.toml crates/cloudsync-server/Cargo.toml                                                 
COPY crates/cloudsync-client/Cargo.toml crates/cloudsync-client/Cargo.toml 

# Create stub source files so cargo can resolve deps                                                                       
RUN mkdir -p crates/cloudsync-common/src && echo "" > crates/cloudsync-common/src/lib.rs \
    && mkdir -p crates/cloudsync-server/src && echo "fn main() {}" > crates/cloudsync-server/src/main.rs && echo "" > crates/cloudsync-server/src/lib.rs \
    && mkdir -p crates/cloudsync-client/src && echo "fn main() {}" > crates/cloudsync-client/src/main.rs && echo "" > crates/cloudsync-client/src/lib.rs  

# Build deps only (cached unless Cargo.toml/lock change)                                                                   
RUN cargo build --release -p cloudsync-server

# Copy real source and rebuild
COPY . .
RUN touch crates/cloudsync-common/src/lib.rs crates/cloudsync-server/src/main.rs crates/cloudsync-server/src/lib.rs \
    && cargo build --release -p cloudsync-server

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /app/target/release/cloudsync-server /app/cloudsync-server

EXPOSE 3050

CMD ["/app/cloudsync-server"]