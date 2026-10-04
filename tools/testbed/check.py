#!/usr/bin/env python3
"""Smoke-checks the running testbed's SMTP side and Stalwart's JMAP endpoint.

- Mailpit: SMTP with STARTTLS (testbed CA verified) on 17025, then the message is found
  through the HTTP API on 17080 and deleted again.
- Stalwart: submission with STARTTLS on 16587 (bob -> alice), then the message is found in
  alice's INBOX over IMAP; the JMAP session resource answers on 16080.
Synthetic content only. Exits non-zero on the first failure.
"""

import base64
import json
import smtplib
import ssl
import sys
import time
import urllib.request
from email.message import EmailMessage
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import seed  # noqa: E402

UA = "mail-dev-testbed (automated)"


def context():
    return ssl.create_default_context(cafile=str(HERE / "certs/ca.pem"))


def message(sender, recipient, subject):
    msg = EmailMessage()
    msg["From"] = sender
    msg["To"] = recipient
    msg["Subject"] = subject
    msg["Message-ID"] = f"<{subject.replace(' ', '.')}@example.test>"
    msg.set_content("Synthetic testbed check.\n")
    return msg


def http(url, auth=None, method="GET"):
    headers = {"User-Agent": UA}
    if auth:
        headers["Authorization"] = "Basic " + base64.b64encode(auth.encode()).decode()
    request = urllib.request.Request(url, headers=headers, method=method)
    with urllib.request.urlopen(request, timeout=10) as response:
        body = response.read()
        if response.headers.get_content_type() != "application/json":
            return body.decode(errors="replace")
        return json.loads(body)


def check_mailpit():
    subject = f"testbed check {time.time_ns()}"
    with smtplib.SMTP("127.0.0.1", 17025, local_hostname="localhost", timeout=10) as smtp:
        smtp.starttls(context=context())
        smtp.login("anyone", "anything")
        smtp.send_message(message("sender@example.test", "someone@example.test", subject))
    for _ in range(20):
        found = http("http://127.0.0.1:17080/api/v1/search?query=" + urllib.request.quote(f'subject:"{subject}"'))
        if found and found.get("messages_count", 0) >= 1:
            http("http://127.0.0.1:17080/api/v1/messages", method="DELETE")
            print("mailpit: SMTP+STARTTLS accepted the message; the API found it")
            return
        time.sleep(0.25)
    sys.exit("mailpit: message not found through the API")


def check_stalwart():
    subject = f"stalwart check {time.time_ns()}"
    with smtplib.SMTP("127.0.0.1", 16587, local_hostname="localhost", timeout=10) as smtp:
        smtp.starttls(context=context())
        smtp.login("bob", seed.PASSWORD)
        smtp.send_message(message("bob@example.test", "alice@example.test", subject))
    imap = seed.Imap(16993, "alice")
    imap.command(b"SELECT INBOX")
    for _ in range(40):
        hits = imap.command(b'UID SEARCH SUBJECT "' + subject.encode() + b'"', want=b"SEARCH")
        if hits and len(hits[0].split()) > 2:
            print("stalwart: submission (587, STARTTLS, AUTH PLAIN) delivered to alice's INBOX")
            break
        time.sleep(0.25)
    else:
        sys.exit("stalwart: the submitted message did not reach alice's INBOX")
    imap.logout()
    session = http("http://127.0.0.1:16080/jmap/session", auth=f"alice:{seed.PASSWORD}")
    if "urn:ietf:params:jmap:mail" not in session.get("capabilities", {}):
        sys.exit("stalwart: JMAP session without the mail capability")
    print("stalwart: JMAP session resource answers for alice")


if __name__ == "__main__":
    check_mailpit()
    check_stalwart()
