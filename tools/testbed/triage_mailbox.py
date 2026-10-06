#!/usr/bin/env python3
"""Prepares and shows the testbed user `triage` for tests/selftest/testbed-triage.sealwax-test.

  triage_mailbox.py fill SERVER...  empties every folder of `triage` and appends eight synthetic
                                    messages to INBOX ("Triage 1" to "Triage 8", the newest last;
                                    1 to 5 unread), creating the folder Prosjekter if missing
  triage_mailbox.py show SERVER...  prints each folder's messages as the server holds them

SERVER is dovecot, dovecot-rev1 or stalwart. It touches only `triage`, never starts, stops or
reseeds the testbed, and connects only to 127.0.0.1. The CA is $SEALWAX_TESTBED_CA, else
certs/ca.pem beside this file. All content is synthetic.
"""

import datetime
import os
import re
import sys
from pathlib import Path

import seed

USER = "triage"
FOLDERS = seed.ROLE_FOLDERS + ["Prosjekter"]
COUNT = 8
UNREAD = 5
EXISTS = re.compile(rb"^\* (\d+) EXISTS")
FETCHED = re.compile(rb"^\* \d+ FETCH \(.*FLAGS \(([^)]*)\).*\{(\d+)\}$")


def connect(server: str) -> seed.Imap:
    if server not in seed.SERVERS:
        sys.exit(f"triage_mailbox: unknown server {server}; choose from {', '.join(seed.SERVERS)}")
    ca = os.environ.get("SEALWAX_TESTBED_CA")
    if ca:
        # seed.Imap reads certs/ca.pem under seed.HERE.
        seed.HERE = Path(ca).resolve().parent.parent
    return seed.Imap(seed.SERVERS[server][0], USER)


def decode(name: bytes) -> str:
    return name.decode()


def select(imap: seed.Imap, folder: bytes) -> int:
    tag = imap.tag()
    imap.sock.sendall(tag + b' SELECT "' + folder + b'"\r\n')
    exists = 0
    while True:
        line = imap.reader.readline()
        if not line:
            raise ConnectionError("connection closed")
        match = EXISTS.match(line)
        if match:
            exists = int(match.group(1))
        if line.startswith(tag + b" "):
            if line.split()[1] != b"OK":
                raise RuntimeError(line.decode(errors="replace").strip())
            return exists


def message(i: int, now: datetime.datetime):
    date = now - datetime.timedelta(hours=COUNT + 1 - i)
    body = f"Syntetisk melding {i} for testbed-triage.\r\n"
    headers = [
        f'From: "Synthetic Sender {i}" <sender{i}@example.test>',
        f"To: <{USER}@example.test>",
        f"Subject: Triage {i}",
        "Date: " + date.strftime("%a, %d %b %Y %H:%M:%S +0000"),
        f"Message-ID: <{USER}.testbed-triage.{i}@example.test>",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
    ]
    flags = [] if i <= UNREAD else ["\\Seen"]
    return flags, date, ("\r\n".join(headers) + "\r\n\r\n" + body).encode()


def fill(server: str):
    imap = connect(server)
    imap.ensure_folders(FOLDERS)
    for folder in sorted(imap.folders()):
        try:
            exists = select(imap, folder)
        except RuntimeError:
            continue
        if exists:
            imap.command(b"STORE 1:* +FLAGS.SILENT (\\Deleted)")
            imap.command(b"EXPUNGE")
    now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)
    imap.append_all("INBOX", (message(i, now) for i in range(1, COUNT + 1)))
    imap.logout()
    print(f"triage_mailbox: {server}: every folder emptied, {COUNT} messages in INBOX")


def show(server: str):
    imap = connect(server)
    for folder in sorted(imap.folders()):
        try:
            exists = select(imap, folder)
        except RuntimeError:
            continue
        rows = []
        if exists:
            tag = imap.tag()
            imap.sock.sendall(tag + b" FETCH 1:* (FLAGS BODY.PEEK[HEADER.FIELDS (SUBJECT)])\r\n")
            while True:
                line = imap.reader.readline()
                if not line:
                    raise ConnectionError("connection closed")
                match = FETCHED.match(line.rstrip(b"\r\n"))
                if match:
                    header = imap.reader.read(int(match.group(2))).decode(errors="replace")
                    subject = header.replace("Subject:", "").strip()
                    flags = match.group(1).decode()
                    rows.append(f"{subject} [{flags}]")
                if line.startswith(tag + b" "):
                    break
        print(f"{server} {decode(folder)}: {exists}")
        for row in rows:
            print(f"  {row}")
    imap.logout()


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in ("fill", "show"):
        sys.exit(__doc__)
    for server in sys.argv[2:]:
        (fill if sys.argv[1] == "fill" else show)(server)


if __name__ == "__main__":
    main()
