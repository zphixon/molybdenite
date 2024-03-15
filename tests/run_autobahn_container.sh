case "$1" in
    run_test_server)
        netOpt="-p 9001:9001"
        ;;
    run_test_client)
        execOpt="/opt/pypy/bin/wstest -m fuzzingclient"
        netOpt="--net=host"
        ;;
    *)
        echo "need one of run_test_client or run_test_server"
        exit 1
        ;;
esac

podman run -it --rm \
    -v "${PWD}/config:/config" \
    -v "${PWD}/reports:/reports" \
    $netOpt \
    --name fuzzingserver \
    docker.io/crossbario/autobahn-testsuite \
    $execOpt
