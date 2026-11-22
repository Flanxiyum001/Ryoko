# Ryoko CLI-only OCI package for GitHub Container Registry.
# Native Windows/macOS/Linux desktop installers are distributed via Releases.
# The executable is built by the Linux x64 CI matrix, not inside this image.
FROM ubuntu:22.04
LABEL org.opencontainers.image.title="Ryoko CLI" \
      org.opencontainers.image.description="Experimental Ryoko command-line tools (not the desktop app)" \
      org.opencontainers.image.source="https://github.com/Flanxiyum001/Ryoko" \
      org.opencontainers.image.licenses="Apache-2.0"
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates git \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 ryoko \
    && mkdir -p /workspace \
    && chown ryoko:ryoko /workspace
COPY ryoko /usr/local/bin/ryoko
RUN chmod 0755 /usr/local/bin/ryoko
USER ryoko
WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/ryoko"]
