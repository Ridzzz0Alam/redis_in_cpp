#!/usr/bin/env python3
# Adapted from "Build Your Own Redis with C/C++" (build-your-own.org), 11/test_cmds.py.
# Changes: starts its own server on an OS-chosen port, takes the binary paths as
# arguments, and checks the CLI exit status (1 = error reply).
import argparse
import re
import shlex
import subprocess
import sys

CASES = r'''
$ zscore asdf n1
(nil)
$ zquery xxx 1 asdf 1 10
(arr) len=0
(arr) end
$ zadd zset 1 n1
(int) 1
$ zadd zset 2 n2
(int) 1
$ zadd zset 1.1 n1
(int) 0
$ zscore zset n1
(dbl) 1.1
$ zquery zset 1 "" 0 10
(arr) len=4
(str) n1
(dbl) 1.1
(str) n2
(dbl) 2
(arr) end
$ zquery zset 1.1 "" 1 10
(arr) len=2
(str) n2
(dbl) 2
(arr) end
$ zquery zset 1.1 "" 2 10
(arr) len=0
(arr) end
$ zrem zset adsf
(int) 0
$ zrem zset n1
(int) 1
$ zquery zset 1 "" 0 10
(arr) len=2
(str) n2
(dbl) 2
(arr) end
$ set k v
(nil)
$ get k
(str) v
$ SET k v2
(nil)
$ pexpire k 100000
(int) 1
$ set k v3
(nil)
$ pttl k
(int) -1
$ zquery zset 100 "" -1 1
(arr) len=2
(str) n2
(dbl) 2
(arr) end
$ unknowncmd
(err) 1 unknown command.
'''


def start_server(path):
    """Start the server on an OS-chosen port and return (process, port).

    Binding port 0 inside the server avoids the race of picking a "free" port
    here and hoping nobody takes it before the server binds it.
    """
    server = subprocess.Popen([path, '--port', '0', '--quiet'],
                              stderr=subprocess.PIPE, text=True)
    line = server.stderr.readline()  # "listening on 127.0.0.1:PORT"
    match = re.search(r':(\d+)$', line.strip())
    if not match:
        server.kill()
        rest = server.stderr.read()
        sys.exit(f'server failed to start: {line}{rest}')
    return server, int(match.group(1))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--server', required=True)
    ap.add_argument('--cli', required=True)
    args = ap.parse_args()

    cmds, outputs = [], []
    for line in CASES.splitlines():
        line = line.strip()
        if not line:
            continue
        if line.startswith('$ '):
            cmds.append(line[2:])
            outputs.append('')
        else:
            outputs[-1] += line + '\n'

    server, port = start_server(args.server)
    try:
        for cmd, expect in zip(cmds, outputs):
            # Exit status 1 means "the server replied with an error", which
            # some cases expect; anything else non-zero is a real failure.
            res = subprocess.run([args.cli, '--port', str(port)] + shlex.split(cmd),
                                 capture_output=True, text=True, timeout=10)
            want_rc = 1 if expect.startswith('(err)') else 0
            if res.stdout != expect or res.returncode != want_rc:
                print(f'FAIL: {cmd} (exit {res.returncode}, expected {want_rc})\n'
                      f'--- got ---\n{res.stdout}{res.stderr}--- expected ---\n{expect}')
                return 1
        print(f'all {len(cmds)} commands passed')
        return 0
    finally:
        server.terminate()
        server.wait(timeout=5)


if __name__ == '__main__':
    sys.exit(main())
