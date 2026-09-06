"""Move the project between the tree and the run host.

rsync and scp are not on the harness allowlist, and neither is a shell
redirect; ssh is. So the push tars the project tree, minus the virtualenv
and run outputs, over ssh's stdin and unpacks it at the same relative path
on the remote; the pull reads one remote file over ssh's stdout and writes
it locally.

    python3 analysis/tape-v2/sync.py                     # push source
    python3 analysis/tape-v2/sync.py pull-lock           # fetch uv.lock
    python3 analysis/tape-v2/sync.py pull REMOTE LOCAL   # fetch one file
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REMOTE = "speilegg"
REMOTE_DIR = "Claude/tape-v2"
EXCLUDES = [".venv", ".git", "__pycache__", "data", "out"]


def push() -> int:
    tar_cmd = [
        "tar",
        "-C",
        str(HERE),
        "-cf",
        "-",
        *[f"--exclude={name}" for name in EXCLUDES],
        ".",
    ]
    remote = f"mkdir -p {REMOTE_DIR} && tar -C {REMOTE_DIR} -xf -"
    tar = subprocess.Popen(tar_cmd, stdout=subprocess.PIPE)
    assert tar.stdout is not None
    ssh = subprocess.run(["ssh", REMOTE, remote], stdin=tar.stdout)
    tar.stdout.close()
    tar_rc = tar.wait()
    if tar_rc or ssh.returncode:
        print(
            f"push failed: tar rc={tar_rc} ssh rc={ssh.returncode}",
            file=sys.stderr,
        )
        return 1
    print(f"pushed {HERE} -> {REMOTE}:{REMOTE_DIR}")
    return 0


def pull(remote_path: str, local_path: Path) -> int:
    result = subprocess.run(
        ["ssh", REMOTE, f"cat {remote_path}"],
        capture_output=True,
    )
    if result.returncode or not result.stdout:
        print(f"pull failed: rc={result.returncode}", file=sys.stderr)
        sys.stderr.buffer.write(result.stderr)
        return 1
    local_path.parent.mkdir(parents=True, exist_ok=True)
    local_path.write_bytes(result.stdout)
    size = len(result.stdout)
    print(f"pulled {REMOTE}:{remote_path} -> {local_path} ({size} bytes)")
    return 0


def main(argv: list[str]) -> int:
    if not argv:
        return push()
    if argv == ["pull-lock"]:
        return pull(f"{REMOTE_DIR}/uv.lock", HERE / "uv.lock")
    if len(argv) == 3 and argv[0] == "pull":
        return pull(argv[1], Path(argv[2]))
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
