#!/usr/bin/env bash
# Starts the mail testbed (ML-005): Dovecot 2.4 (full), Dovecot 2.4 (IMAP4rev1 only), Stalwart and
# Mailpit, all on 127.0.0.1. Generates the testbed CA and the localhost certificate on first use and
# configures Stalwart (domain, listeners, certificate, users) through its management API.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

if [[ ! -f certs/ca.pem || ! -f certs/server.pem ]]; then
    mkdir -p certs
    (
        cd certs
        umask 077
        openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 3650 \
            -subj "/CN=Sealwax testbed CA" -keyout ca.key -out ca.pem \
            -addext "basicConstraints=critical,CA:TRUE" \
            -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
        openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=localhost" \
            -keyout server.key -out server.csr 2>/dev/null
        printf '%s\n' 'subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1' \
            'basicConstraints=critical,CA:FALSE' 'keyUsage=critical,digitalSignature' \
            'extendedKeyUsage=serverAuth' >server.ext
        openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 825 \
            -out server.pem -extfile server.ext 2>/dev/null
        rm -f server.csr server.ext ca.srl
        chmod 644 ca.pem server.pem
    )
    echo "testbed: generated certs/ca.pem and certs/server.pem (CN=localhost)"
fi

docker compose up -d --wait

python3 stalwart_setup.py

echo "testbed: up"
echo "  dovecot       imaps 127.0.0.1:14993  imap+STARTTLS 127.0.0.1:14143  pop3s 14995  pop3+STLS 14110"
echo "  dovecot-rev1  imaps 127.0.0.1:15993  imap+STARTTLS 127.0.0.1:15143  (IMAP4rev1 only)"
echo "  stalwart      imaps 127.0.0.1:16993  imap+STARTTLS 127.0.0.1:16143  pop3s 16995  pop3+STLS 16110  smtp 16025  submission 16587  submissions 16465  http/jmap 16080"
echo "  mailpit       smtp+STARTTLS 127.0.0.1:17025  http/api 127.0.0.1:17080"
echo "  users alice bob perf rand fault idle spike ops opsfault triage carol dave outbox outboxfault newmail,"
echo "  password testbed-password; CA certs/ca.pem"
