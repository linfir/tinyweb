#!/usr/bin/env python3

import datetime
import random

N = 10_000  # nombre de tests
MAX_SECS = 7258118400  # de 1970 à 2199

DAYS = "Mon Tue Wed Thu Fri Sat Sun".split()
MONTHS = "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split()


def http_date(t):
    dt = datetime.datetime.fromtimestamp(t, tz=datetime.timezone.utc)
    x = f"{DAYS[dt.weekday()]}, {dt.day:02d} {MONTHS[dt.month - 1]} {dt.year}"
    x += f" {dt.hour:02d}:{dt.minute:02d}:{dt.second:02d} GMT"
    return x


timestamps = sorted(set([0] + [random.randint(0, MAX_SECS) for _ in range(N)]))

for t in timestamps:
    print(f"{t} {http_date(t)}")
