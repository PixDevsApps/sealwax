#!/usr/bin/env python3
"""Configures the testbed Stalwart through its JMAP management API. Idempotent.

Creates the testbed certificate (certs/server.pem), the domain example.test, plaintext IMAP (143)
and submission (587) listeners with STARTTLS, and the test users. Stalwart is restarted when a
listener was added, because new listeners only bind at start-up.
"""

import base64
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
URL = "http://127.0.0.1:16080/jmap/"
ADMIN = "admin:testbed-admin"
DOMAIN = "example.test"
USERS = ["alice", "bob", "perf", "rand", "fault", "idle", "spike"]
PASSWORD = "testbed-password"


def call(calls):
    body = json.dumps(
        {"using": ["urn:ietf:params:jmap:core", "urn:stalwart:jmap"], "methodCalls": calls}
    ).encode()
    request = urllib.request.Request(
        URL,
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": "Basic " + base64.b64encode(ADMIN.encode()).decode(),
            "User-Agent": "mail-dev-testbed (automated)",
        },
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        responses = json.load(response)["methodResponses"]
    for name, result, _ in responses:
        if name == "error" or result.get("notCreated") or result.get("notUpdated"):
            sys.exit(f"stalwart_setup: {name} failed: {json.dumps(result)}")
    return responses


def get_all(kind):
    responses = call(
        [
            [f"x:{kind}/query", {"filter": {}}, "q"],
            [
                f"x:{kind}/get",
                {"#ids": {"resultOf": "q", "name": f"x:{kind}/query", "path": "/ids"}},
                "g",
            ],
        ]
    )
    return responses[1][1]["list"]


def wait_for_api():
    deadline = time.monotonic() + 60
    while True:
        try:
            get_all("Domain")
            return
        except (urllib.error.URLError, ConnectionError, OSError):
            if time.monotonic() > deadline:
                sys.exit("stalwart_setup: the management API did not come up within 60 s")
            time.sleep(0.5)


def main():
    wait_for_api()
    cert = (HERE / "certs/server.pem").read_text()
    key = (HERE / "certs/server.key").read_text()

    certificates = get_all("Certificate")
    if certificates:
        cert_id = certificates[0]["id"]
    else:
        created = call(
            [
                [
                    "x:Certificate/set",
                    {
                        "create": {
                            "c": {
                                "certificate": {"@type": "Text", "value": cert},
                                "privateKey": {"@type": "Text", "secret": key},
                            }
                        }
                    },
                    "1",
                ]
            ]
        )
        cert_id = created[0][1]["created"]["c"]["id"]

    domains = [d for d in get_all("Domain") if d["name"] == DOMAIN]
    if domains:
        domain_id = domains[0]["id"]
    else:
        created = call(
            [
                [
                    "x:Domain/set",
                    {
                        "create": {
                            "d": {
                                "name": DOMAIN,
                                "certificateManagement": {"@type": "Manual"},
                                "dkimManagement": {"@type": "Manual"},
                                "dnsManagement": {"@type": "Manual"},
                            }
                        }
                    },
                    "1",
                ]
            ]
        )
        domain_id = created[0][1]["created"]["d"]["id"]

    call(
        [
            [
                "x:SystemSettings/set",
                {
                    "update": {
                        "singleton": {
                            "defaultHostname": "mail.example.test",
                            "defaultCertificateId": cert_id,
                            "defaultDomainId": domain_id,
                        }
                    }
                },
                "1",
            ]
        ]
    )

    listeners = {listener["name"] for listener in get_all("NetworkListener")}
    wanted = {"imap": (143, "imap"), "submission": (587, "smtp")}
    missing = {
        name: {
            "name": name,
            "bind": {f"[::]:{port}": True},
            "protocol": protocol,
            "useTls": True,
            "tlsImplicit": False,
        }
        for name, (port, protocol) in wanted.items()
        if name not in listeners
    }
    if missing:
        call([["x:NetworkListener/set", {"create": missing}, "1"]])

    accounts = {account["name"] for account in get_all("Account")}
    users = {
        name: {
            "@type": "User",
            "name": name,
            "domainId": domain_id,
            "credentials": {"0": {"@type": "Password", "secret": PASSWORD}},
            "roles": {"@type": "User"},
            "permissions": {"@type": "Inherit"},
            "encryptionAtRest": {"@type": "Disabled"},
        }
        for name in USERS
        if name not in accounts
    }
    if users:
        call([["x:Account/set", {"create": users}, "1"]])

    call(
        [
            [
                "x:Action/set",
                {
                    "create": {
                        "settings": {"@type": "ReloadSettings"},
                        "tls": {"@type": "ReloadTlsCertificates"},
                    }
                },
                "1",
            ]
        ]
    )

    if missing:
        subprocess.run(
            ["docker", "compose", "restart", "--timeout", "10", "stalwart"], cwd=HERE, check=True
        )
        wait_for_api()
    print(f"stalwart_setup: ok ({len(users)} users created, {len(missing)} listeners added)")


if __name__ == "__main__":
    main()
