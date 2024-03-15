podman run -it --rm \
    -v "${PWD}/config:/config" \
    -v "${PWD}/reports:/reports" \
    -p 9001:9001 \
    --name fuzzingserver \
    docker.io/crossbario/autobahn-testsuite
