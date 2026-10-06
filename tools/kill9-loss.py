#!/usr/bin/env python3
"""The loss of one kill -9 run (tools/kill9-compose.sh), or the summary of all of them.

A run: kill9-loss.py TYPED_LOG RECOVERED KILLED_MS STARTED_MS RUN prints one tab-separated row:
the run, seconds from the first typed character to the kill, characters typed before the kill,
characters the reopened draft holds, characters lost, seconds of typing lost (from the kill back
to when the last recovered character was typed), and whether the recovered text is exactly a
prefix of what was typed.

The summary: kill9-loss.py --summary SUMMARY_TSV.
"""

import sys

LETTERS = "abcdefghijklmnopqrstuvwxyz"


def run(log, recovered, killed, started, number):
    lines = open(log).read().splitlines()[1:]
    times = [int(line.split()[1]) for line in lines if len(line.split()) == 2]
    typed = [time for time in times if time <= killed]
    expected = "".join(LETTERS[index % len(LETTERS)] for index in range(len(times)))
    text = open(recovered).read().split("\n", 1)[1].strip()
    count = len(text)
    prefix = expected.startswith(text)
    if count == 0:
        lost = (killed - started) / 1000
    else:
        lost = (killed - times[count - 1]) / 1000
    print(
        f"{number}\t{(killed - started) / 1000:.1f}\t{len(typed)}\t{count}\t"
        f"{len(typed) - count}\t{lost:.2f}\t{prefix}"
    )


def summary(path):
    rows = [line.split("\t") for line in open(path).read().splitlines()[1:]]
    lost = [float(row[5]) for row in rows]
    prefixes = all(row[6] == "True" for row in rows)
    verdict = "met" if lost and max(lost) <= 5 and prefixes else "NOT met"
    print(
        f"runs {len(rows)}; seconds lost: max {max(lost):.2f}, mean {sum(lost) / len(lost):.2f}; "
        f"every recovered text a prefix of what was typed: {prefixes}; the 5 s bar: {verdict}"
    )


if __name__ == "__main__":
    if sys.argv[1] == "--summary":
        summary(sys.argv[2])
    else:
        run(sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5])
