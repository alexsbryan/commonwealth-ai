"""engine-swap's HEADROOM check: memory available for a live run's own need.

The bar is what the run loads, not which host it is on: argv[1] in GB, else
16 GB, the addendum's ceiling for one live-run model (12 GB, "never a model
over 12 GB beside the deployed daemon") plus 4 GB for its KV cache, compute
buffers and the scratch daemon. A row that loads more (f3's hybrid model)
passes its own number: `scripts/ralph-check.sh headroom 40`.

Available memory is the kernel's own estimate where it has one: Linux's
MemAvailable (/proc/meminfo); on macOS, free + inactive + speculative pages
from vm_stat (what can be handed out without swapping). Memory a deployed
daemon already holds is excluded on both, so the bar never has to guess a
host's resident model (the 60 GB bar of 2026-10-09 did, for the Halo).
Exit 0 at or above the bar, 1 below it, 2 when memory cannot be read: an
unreadable reading is refused, never taken as enough."""
import re
import subprocess
import sys

DEFAULT_NEED_GB = 16


def linux_available_bytes():
    for line in open("/proc/meminfo"):
        if line.startswith("MemAvailable:"):
            return int(line.split()[1]) * 1024
    return None


def macos_available_bytes():
    out = subprocess.run(["vm_stat"], capture_output=True, text=True, check=True).stdout
    page = re.search(r"page size of (\d+) bytes", out)
    pages = {}
    for line in out.splitlines()[1:]:
        key, _, value = line.partition(":")
        value = value.strip().rstrip(".")
        if value.isdigit():
            pages[key.strip()] = int(value)
    want = ("Pages free", "Pages inactive", "Pages speculative")
    if not page or any(k not in pages for k in want):
        return None
    return sum(pages[k] for k in want) * int(page.group(1))


def main():
    need = float(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_NEED_GB
    if sys.platform.startswith("linux"):
        source, available = "MemAvailable", linux_available_bytes()
    elif sys.platform == "darwin":
        source, available = "vm_stat free+inactive+speculative", macos_available_bytes()
    else:
        print(f"headroom: no memory reading on {sys.platform}")
        return 2
    if available is None:
        print(f"headroom: {source} unreadable")
        return 2
    gb = available / 2**30
    print(f"available {gb:.0f} GB ({source}), need {need:g} GB")
    return 0 if gb >= need else 1


sys.exit(main())
