#!/usr/bin/env python3
"""Build the five 1x1 PNG fixtures that isolate each PNG first-chunk path."""
import struct
import zlib


def chunk(kind, data):
    return (struct.pack('>I', len(data)) + kind + data
            + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff))


SIG = b'\x89PNG\r\n\x1a\n'
IHDR = struct.pack('>IIBBBBB', 1, 1, 8, 0, 0, 0, 0)   # 1x1, 8-bit greyscale
IDAT = zlib.compress(b'\x00\x00', 9)                  # filter byte + 1 pixel
TAIL = chunk(b'IDAT', IDAT) + chunk(b'IEND', b'')

files = {
    # first chunk is IHDR and the body is valid
    't_valid.png': SIG + chunk(b'IHDR', IHDR) + TAIL,
    # Apple CgBI ahead of a perfectly valid IHDR
    't_cgbi.png': SIG + chunk(b'CgBI', b'\x50\x00\x20\x02') + chunk(b'IHDR', IHDR) + TAIL,
    # some other unknown critical chunk first
    't_unknown_first.png': SIG + chunk(b'ZzZz', b'\x01\x02') + chunk(b'IHDR', IHDR) + TAIL,
    # the same unknown critical chunk, but second
    't_unknown_second.png': SIG + chunk(b'IHDR', IHDR) + chunk(b'ZzZz', b'\x01\x02') + TAIL,
    # IHDR is first but its body is genuinely malformed (width 0)
    't_bad_ihdr.png': SIG + chunk(b'IHDR', struct.pack('>IIBBBBB', 0, 1, 8, 0, 0, 0, 0)) + TAIL,
}

for name, data in files.items():
    with open(name, 'wb') as f:
        f.write(data)
    print(f'{name}: {len(data)} bytes')
