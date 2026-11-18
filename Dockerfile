FROM alpine:3.22
RUN apk add --no-cache ca-certificates
COPY zig/zig-out/bin/oxap /usr/local/bin/oxap
ENTRYPOINT ["oxap"]
