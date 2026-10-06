#!/usr/bin/env python3
"""New mail in background mode, end to end (M7 phase B): runs
tests/selftest/testbed-notify.sealwax-test headless and, once the script has closed Sealwax's
window into background mode, APPENDs one fictional message to Stalwart's newmail INBOX as another
IMAP client. Prints the time from the APPEND's OK to the line where the script saw the
notification (the self-test's output, read as it comes; resolution about 10 ms) and the script's
result.

Usage: python3 tools/testbed/notify_e2e.py [DISPLAY]   (default display 19; the testbed running)
"""

import datetime
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
from seed import Imap  # noqa: E402

STALWART = 16993
USER = "newmail"
MARKER = "assert-notify 1 summary"
SEEN = "wait-until assert-notify-count 2"


def message(serial: str) -> bytes:
    return (
        "From: Ada Example <ada@example.test>\r\n"
        "To: Newmail <newmail@example.test>\r\n"
        "Subject: Testbed end-to-end check\r\n"
        f"Message-ID: <notify-e2e-{serial}@example.test>\r\n"
        f"Date: {datetime.datetime.now(datetime.timezone.utc).strftime('%a, %d %b %Y %H:%M:%S +0000')}\r\n"
        "MIME-Version: 1.0\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n"
        "\r\n"
        "A fictional message for the end-to-end notification check.\r\n"
    ).encode()


def main() -> int:
    display = sys.argv[1] if len(sys.argv) > 1 else "19"
    script = ROOT / "tests/selftest/testbed-notify.sealwax-test"
    run = subprocess.Popen(
        [str(ROOT / "tools/headless.sh"), display, str(ROOT / "target/debug/sealwax"),
         "--self-test", str(script)],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1,
    )
    appended_at = None
    seen_at = None
    lines = []
    for line in run.stdout:
        lines.append(line)
        if line.startswith(("ok ", "not ok ")):
            print(line, end="")
        if appended_at is None and line.startswith("ok ") and MARKER in line:
            imap = Imap(STALWART, USER)
            serial = str(time.time_ns())
            imap.append_all("INBOX", [((), datetime.datetime.now(datetime.timezone.utc),
                                       message(serial))])
            appended_at = time.monotonic()
            imap.logout()
            print(f"# appended one message to {USER}'s INBOX on Stalwart")
        if seen_at is None and appended_at is not None and line.startswith("ok ") and SEEN in line:
            seen_at = time.monotonic()
    code = run.wait()
    summary = [line for line in lines if line.startswith("# ") and "steps passed" in line]
    print(summary[-1] if summary else "# no summary", end="" if summary else "\n")
    if appended_at is not None and seen_at is not None:
        print(f"# APPEND OK → notification seen by the script: {(seen_at - appended_at) * 1000:.0f} ms")
    if code != 0:
        log = ROOT / ".local/testbed-notify.log"
        log.write_text("".join(lines))
        print(f"# the self-test failed (exit {code}); its output: {log}")
    return code


if __name__ == "__main__":
    sys.exit(main())
