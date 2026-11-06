#!/usr/bin/env python3
# Adapted from "Build Your Own Redis with C/C++" (build-your-own.org), 11/test_cmds.py.
# Changes: starts its own server on a free port and takes the binary paths as arguments.
import argparse
import shlex
import socket
import subprocess
import sys
import time

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
$ unknowncmd
(err) 1 unknown command.
'''


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


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

    port = free_port()
    server = subprocess.Popen([args.server, '--port', str(port), '--quiet'])
    try:
        for _ in range(100):  # wait until it accepts connections
            try:
                socket.create_connection(('127.0.0.1', port), timeout=0.1).close()
                break
            except OSError:
                time.sleep(0.05)
        for cmd, expect in zip(cmds, outputs):
            out = subprocess.check_output(
                [args.cli, '--port', str(port)] + shlex.split(cmd)).decode()
            if out != expect:
                print(f'FAIL: {cmd}\n--- got ---\n{out}--- expected ---\n{expect}')
                return 1
        print(f'all {len(cmds)} commands passed')
        return 0
    finally:
        server.terminate()
        server.wait(timeout=5)


if __name__ == '__main__':
    sys.exit(main())
