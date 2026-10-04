#!/usr/bin/env python3
"""Seeds the testbed IMAP servers with synthetic test data. Idempotent.

On every IMAP server (dovecot, dovecot-rev1, stalwart) it creates the standard folders (Norwegian
names: Utkast, Sendt, Søppelpost, Papirkurv, Arkiv) where the server does not provide them, plus
non-ASCII folders that need modified UTF-7 on the wire, then appends:
  - alice: a small mixed mailbox (threads, flags, attachments) across several folders;
  - perf:  PERF_COUNT synthetic messages in INBOX (default 10,000) for the sync measurements.
All content is synthetic. Usage: seed.py [--perf-count N] [server ...]
"""

import argparse
import base64
import datetime
import re
import socket
import ssl
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
PASSWORD = "testbed-password"
SERVERS = {
    # name: (imaps port, folders the seed must create because the server does not configure them)
    "dovecot": (14993, False),
    "dovecot-rev1": (15993, True),
    "stalwart": (16993, True),
}
ROLE_FOLDERS = ["Utkast", "Sendt", "Søppelpost", "Papirkurv", "Arkiv"]
EXTRA_FOLDERS = [
    "Økonomi & regnskap",
    "Økonomi & regnskap/Kvitteringer",
    "Prosjekter",
    "Prosjekter/Høst 📬 2026",
]
LIST_LINE = re.compile(rb'^\* LIST \([^)]*\) (?:NIL|"(?:[^"\\]|\\.)*") (.*)$')
BASE_DATE = datetime.datetime(2026, 1, 1, 8, 0, tzinfo=datetime.timezone.utc)
WORDS = (
    "alpha bravo charlie delta echo foxtrot golf hotel india juliett kilo lima mike november "
    "oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu fjord bre "
    "skog elv vann hytte ski"
).split()


def mutf7_encode(name: str) -> str:
    out, pending = [], []

    def flush():
        if pending:
            raw = "".join(pending).encode("utf-16-be")
            out.append("&" + base64.b64encode(raw).decode().rstrip("=").replace("/", ",") + "-")
            pending.clear()

    for ch in name:
        if 0x20 <= ord(ch) <= 0x7E:
            flush()
            out.append("&-" if ch == "&" else ch)
        else:
            pending.append(ch)
    flush()
    return "".join(out)


def quote(mailbox: str) -> bytes:
    encoded = mutf7_encode(mailbox)
    return ('"' + encoded.replace("\\", "\\\\").replace('"', '\\"') + '"').encode()


class Imap:
    def __init__(self, port: int, user: str):
        context = ssl.create_default_context(cafile=str(HERE / "certs/ca.pem"))
        deadline = time.monotonic() + 60
        while True:
            try:
                raw = socket.create_connection(("127.0.0.1", port), timeout=30)
                self.sock = context.wrap_socket(raw, server_hostname="localhost")
                self.reader = self.sock.makefile("rb")
                greeting = self.reader.readline()
                if greeting.startswith(b"* OK"):
                    break
                raise ConnectionError(greeting)
            except (OSError, ConnectionError, ssl.SSLError):
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.5)
        self.counter = 0
        self.caps = set(self.command(b"CAPABILITY", want=b"CAPABILITY")[0].split()[2:])
        self.command(b"LOGIN " + user.encode() + b" " + PASSWORD.encode())
        self.caps = set(self.command(b"CAPABILITY", want=b"CAPABILITY")[0].split()[2:])

    def tag(self) -> bytes:
        self.counter += 1
        return b"s%d" % self.counter

    def read_until(self, tag: bytes, want: bytes | None = None):
        found = []
        while True:
            line = self.reader.readline()
            if not line:
                raise ConnectionError("connection closed")
            if line.startswith(tag + b" "):
                if not line.split()[1] == b"OK":
                    raise RuntimeError(line.decode(errors="replace").strip())
                return found
            if want and line.startswith(b"* ") and line.split()[1:2] == [want]:
                found.append(line.rstrip(b"\r\n"))

    def read_completion(self) -> bytes:
        """Reads up to the next tagged response and returns its tag. Servers may complete
        pipelined commands out of order (Stalwart does), so callers track tags as a set."""
        while True:
            line = self.reader.readline()
            if not line:
                raise ConnectionError("connection closed")
            if not line.startswith((b"* ", b"+")):
                tag, status = line.split()[:2]
                if status != b"OK":
                    raise RuntimeError(line.decode(errors="replace").strip())
                return tag

    def command(self, command: bytes, want: bytes | None = None):
        tag = self.tag()
        self.sock.sendall(tag + b" " + command + b"\r\n")
        return self.read_until(tag, want)

    def folders(self) -> set[bytes]:
        names = set()
        for line in self.command(b'LIST "" "*"', want=b"LIST"):
            match = LIST_LINE.match(line)
            if not match:
                raise RuntimeError(f"unexpected LIST line: {line!r}")
            name = match.group(1)
            if name.startswith(b'"'):
                name = name[1:-1].replace(b'\\"', b'"').replace(b"\\\\", b"\\")
            names.add(name)
        return names

    def ensure_folders(self, folders):
        existing = self.folders()
        for folder in folders:
            if mutf7_encode(folder).encode() not in existing:
                self.command(b"CREATE " + quote(folder))
                self.command(b"SUBSCRIBE " + quote(folder))

    def count(self, folder: str) -> int:
        line = self.command(b"STATUS " + quote(folder) + b" (MESSAGES)", want=b"STATUS")[0]
        return int(line.rsplit(b"MESSAGES ", 1)[1].split(b")")[0])

    def append_all(self, folder: str, messages):
        """Appends (flags, date, bytes) tuples; pipelined with LITERAL+ when offered."""
        literal_plus = b"LITERAL+" in self.caps
        window, pending = 64, set()
        for flags, date, body in messages:
            tag = self.tag()
            head = (
                tag
                + b" APPEND "
                + quote(folder)
                + b" ("
                + " ".join(flags).encode()
                + b') "'
                + date.strftime("%d-%b-%Y %H:%M:%S +0000").encode()
                + b'" {%d' % len(body)
            )
            if literal_plus:
                self.sock.sendall(head + b"+}\r\n" + body + b"\r\n")
                pending.add(tag)
                if len(pending) >= window:
                    pending.remove(self.read_completion())
            else:
                self.sock.sendall(head + b"}\r\n")
                line = self.reader.readline()
                if not line.startswith(b"+"):
                    raise RuntimeError(line.decode(errors="replace").strip())
                self.sock.sendall(body + b"\r\n")
                self.read_until(tag)
        while pending:
            pending.remove(self.read_completion())

    def logout(self):
        try:
            self.command(b"LOGOUT")
        except (ConnectionError, OSError):
            pass
        self.sock.close()


def text(seed: int, words: int) -> str:
    out, state = [], seed * 2654435761 % 2**32
    for i in range(words):
        state = (state * 1103515245 + 12345) % 2**31
        out.append(WORDS[state % len(WORDS)])
        if i % 14 == 13:
            out.append("\r\n")
    return " ".join(out).replace(" \r\n ", "\r\n")


def message(user: str, i: int, folder_tag: str = "inbox"):
    date = BASE_DATE + datetime.timedelta(minutes=7 * i)
    sender = i % 50
    headers = [
        f'From: "Synthetic Sender {sender}" <sender{sender}@example.test>',
        f"To: <{user}@example.test>",
        f"Subject: Synthetic message {i} ({folder_tag})"
        if i % 5
        else f"Subject: =?UTF-8?B?{base64.b64encode(f'Hilsen fra Bærum {i}'.encode()).decode()}?=",
        "Date: " + date.strftime("%a, %d %b %Y %H:%M:%S +0000"),
        f"Message-ID: <{user}.{folder_tag}.{i}@example.test>",
    ]
    if i % 4 == 3:
        parent = f"<{user}.{folder_tag}.{i - 1}@example.test>"
        grandparent = f"<{user}.{folder_tag}.{i - 2}@example.test>"
        headers += [f"In-Reply-To: {parent}", f"References: {grandparent} {parent}"]
    if i % 10 == 0:
        headers.append(f"List-Unsubscribe: <mailto:unsubscribe@example.test?subject=u{i}>")
    headers.append("MIME-Version: 1.0")
    body = text(i, 40 + (i * 37) % 400)
    if i % 7 == 0:
        attachment = base64.encodebytes(text(i + 1, 120).encode()).decode().replace("\n", "\r\n")
        headers.append('Content-Type: multipart/mixed; boundary="b1"')
        payload = (
            "--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n"
            + body
            + "\r\n--b1\r\nContent-Type: application/octet-stream\r\n"
            + f'Content-Disposition: attachment; filename="notes-{i}.txt"\r\n'
            + "Content-Transfer-Encoding: base64\r\n\r\n"
            + attachment
            + "--b1--\r\n"
        )
    elif i % 3 == 0:
        headers.append('Content-Type: multipart/alternative; boundary="b2"')
        payload = (
            "--b2\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n"
            + body
            + "\r\n--b2\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>"
            + body
            + "</p>\r\n--b2--\r\n"
        )
    else:
        headers.append("Content-Type: text/plain; charset=utf-8")
        payload = body + "\r\n"
    flags = []
    if i % 3:
        flags.append("\\Seen")
    if i % 11 == 0:
        flags.append("\\Flagged")
    return flags, date, ("\r\n".join(headers) + "\r\n\r\n" + payload).encode()


def seed_server(name: str, perf_count: int):
    port, create_roles = SERVERS[name]
    started = time.monotonic()

    alice = Imap(port, "alice")
    alice.ensure_folders((ROLE_FOLDERS if create_roles else []) + EXTRA_FOLDERS)
    plan = {
        "INBOX": 30,
        "Sendt": 5,
        "Papirkurv": 3,
        "Søppelpost": 2,
        "Økonomi & regnskap/Kvitteringer": 5,
        "Prosjekter/Høst 📬 2026": 3,
    }
    for folder, count in plan.items():
        if alice.count(folder) == 0:
            tag = mutf7_encode(folder).replace("/", "_").replace(" ", "_").replace("&", "")
            alice.append_all(folder, (message("alice", i, tag) for i in range(1, count + 1)))
    alice.logout()

    for user in ["bob", "rand", "fault", "idle", "spike"]:
        account = Imap(port, user)
        account.ensure_folders(ROLE_FOLDERS if create_roles else [])
        account.logout()

    perf = Imap(port, "perf")
    perf.ensure_folders((ROLE_FOLDERS if create_roles else []) + EXTRA_FOLDERS)
    have = perf.count("INBOX")
    if have not in (0, perf_count):
        sys.exit(
            f"seed: {name} perf INBOX holds {have} messages, expected 0 or {perf_count}; "
            "run down.sh (it removes the volumes) and up.sh again"
        )
    if have == 0:
        perf.append_all("INBOX", (message("perf", i) for i in range(1, perf_count + 1)))
    perf.logout()
    print(f"seed: {name} done in {time.monotonic() - started:.1f} s (perf INBOX {perf_count})")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--perf-count", type=int, default=10_000)
    parser.add_argument("servers", nargs="*", default=list(SERVERS))
    args = parser.parse_args()
    for name in args.servers:
        if name not in SERVERS:
            sys.exit(f"seed: unknown server {name}; choose from {', '.join(SERVERS)}")
        seed_server(name, args.perf_count)


if __name__ == "__main__":
    main()
